//! Durable execution state and rebuildable query catalog.
//!
//! Lifecycle JSONL files are authoritative. SQLite is a disposable projection
//! used for history, task-tree, lease, and full-text queries.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{FileSessionStore, SessionResult};

pub const EXECUTION_LEDGER_SCHEMA_VERSION: &str = "openagent.execution_event.v1";
pub const EXECUTION_RECORD_SCHEMA_VERSION: &str = "openagent.execution.v1";
pub const EFFECT_RECEIPT_SCHEMA_VERSION: &str = "openagent.effect_receipt.v1";
pub const SESSION_CATALOG_SCHEMA_VERSION: u64 = 1;

const EXECUTION_LEDGER_FILE: &str = "lifecycle.jsonl";
const EXECUTION_LEDGER_LOCK_FILE: &str = "lifecycle.lock";
const RUNTIME_STATE_DIR: &str = ".openagent-runtime";
const EFFECT_RECEIPT_DIR: &str = "effects";
const SESSION_CATALOG_FILE: &str = "session_catalog.sqlite3";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Queued,
    Running,
    #[serde(alias = "interrupting", alias = "blocked")]
    Waiting,
    Completed,
    Failed,
    #[serde(alias = "canceled", alias = "expired")]
    Cancelled,
    #[default]
    Interrupted,
}

impl ExecutionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
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

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    #[must_use]
    pub fn from_runtime(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "queued" | "pending" => Self::Queued,
            "running" | "in_progress" | "streaming" | "retrying" => Self::Running,
            "waiting" | "interrupting" | "waiting_approval" | "waiting_question"
            | "pending_approval" | "pending_question" | "blocked" | "cancel_requested" => {
                Self::Waiting
            }
            "completed" | "complete" | "done" | "success" => Self::Completed,
            "failed" | "error" => Self::Failed,
            "cancelled" | "canceled" | "expired" => Self::Cancelled,
            _ => Self::Interrupted,
        }
    }

    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Queued => matches!(
                next,
                Self::Running | Self::Waiting | Self::Failed | Self::Cancelled | Self::Interrupted
            ),
            Self::Running => matches!(
                next,
                Self::Waiting
                    | Self::Completed
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Interrupted
            ),
            Self::Waiting => matches!(
                next,
                Self::Queued
                    | Self::Running
                    | Self::Completed
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Interrupted
            ),
            Self::Failed | Self::Cancelled | Self::Interrupted => next == Self::Queued,
            Self::Completed => false,
        }
    }
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    Session,
    Turn,
    Task,
    Approval,
    Question,
}

impl ExecutionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Turn => "turn",
            Self::Task => "task",
            Self::Approval => "approval",
            Self::Question => "question",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    #[default]
    Scheduling,
    Provider,
    Tool,
    Approval,
    Question,
    Compaction,
    Subagent,
    Finalize,
}

impl ExecutionPhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduling => "scheduling",
            Self::Provider => "provider",
            Self::Tool => "tool",
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Compaction => "compaction",
            Self::Subagent => "subagent",
            Self::Finalize => "finalize",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDisposition {
    Resume,
    Retry,
    Interrupt,
    Ignore,
}

impl RecoveryDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Retry => "retry",
            Self::Interrupt => "interrupt",
            Self::Ignore => "ignore",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionLease {
    pub owner_id: String,
    pub claimed_at_ms: u64,
    pub heartbeat_at_ms: u64,
    pub expires_at_ms: u64,
}

impl ExecutionLease {
    #[must_use]
    pub fn new(owner_id: impl Into<String>, claimed_at_ms: u64, ttl_ms: u64) -> ExecutionLease {
        Self {
            owner_id: owner_id.into(),
            claimed_at_ms,
            heartbeat_at_ms: claimed_at_ms,
            expires_at_ms: claimed_at_ms.saturating_add(ttl_ms.max(1)),
        }
    }

    #[must_use]
    pub const fn is_live(&self, now_ms: u64) -> bool {
        self.expires_at_ms > now_ms
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Claimed,
    Committed,
    Failed,
}

impl EffectState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EffectReceipt {
    pub schema_version: String,
    pub idempotency_key: String,
    pub execution_id: String,
    pub phase: ExecutionPhase,
    pub state: EffectState,
    pub claimed_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DurableExecutionRecord {
    pub schema_version: String,
    pub execution_id: String,
    pub session_id: String,
    pub kind: ExecutionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_execution_id: Option<String>,
    pub status: ExecutionStatus,
    pub phase: ExecutionPhase,
    pub attempt: u32,
    pub idempotency_key: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<ExecutionLease>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryDisposition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<EffectReceipt>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
pub struct NewExecution {
    pub execution_id: String,
    pub session_id: String,
    pub kind: ExecutionKind,
    pub parent_execution_id: Option<String>,
    pub status: ExecutionStatus,
    pub phase: ExecutionPhase,
    pub attempt: u32,
    pub idempotency_key: String,
    pub lease: Option<ExecutionLease>,
    pub metadata: BTreeMap<String, Value>,
}

impl NewExecution {
    #[must_use]
    pub fn turn(
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            execution_id: turn_id.into(),
            session_id: session_id.into(),
            kind: ExecutionKind::Turn,
            parent_execution_id: None,
            status: ExecutionStatus::Queued,
            phase: ExecutionPhase::Scheduling,
            attempt: 1,
            idempotency_key: idempotency_key.into(),
            lease: None,
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExecutionLifecycleEvent {
    pub schema_version: String,
    pub event_id: String,
    pub seq: u64,
    pub event_type: String,
    pub timestamp_ms: u64,
    pub session_id: String,
    pub execution_id: String,
    pub record: DurableExecutionRecord,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateExecutionResult {
    pub record: DurableExecutionRecord,
    pub deduplicated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EffectClaim {
    Acquired(EffectReceipt),
    AlreadyCommitted(EffectReceipt),
    Uncertain(EffectReceipt),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryDecision {
    pub disposition: RecoveryDisposition,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryPolicy {
    pub max_attempts: u32,
    pub lease_timeout_ms: u64,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            lease_timeout_ms: 30_000,
        }
    }
}

impl RecoveryPolicy {
    #[must_use]
    pub fn classify(&self, record: &DurableExecutionRecord, now_ms: u64) -> RecoveryDecision {
        if record.status.is_terminal() {
            return RecoveryDecision {
                disposition: RecoveryDisposition::Ignore,
                reason: "terminal".to_string(),
            };
        }
        if record
            .lease
            .as_ref()
            .is_some_and(|lease| lease.is_live(now_ms))
        {
            return RecoveryDecision {
                disposition: RecoveryDisposition::Ignore,
                reason: "live_lease".to_string(),
            };
        }
        match record.status {
            ExecutionStatus::Queued | ExecutionStatus::Waiting => RecoveryDecision {
                disposition: RecoveryDisposition::Resume,
                reason: if record.status == ExecutionStatus::Queued {
                    "durable_queue"
                } else {
                    "durable_wait"
                }
                .to_string(),
            },
            ExecutionStatus::Running => self.classify_running(record),
            ExecutionStatus::Completed
            | ExecutionStatus::Failed
            | ExecutionStatus::Cancelled
            | ExecutionStatus::Interrupted => RecoveryDecision {
                disposition: RecoveryDisposition::Ignore,
                reason: "terminal".to_string(),
            },
        }
    }

    fn classify_running(&self, record: &DurableExecutionRecord) -> RecoveryDecision {
        if record.phase == ExecutionPhase::Tool {
            return match record.effect.as_ref().map(|effect| effect.state) {
                Some(EffectState::Committed) => RecoveryDecision {
                    disposition: RecoveryDisposition::Resume,
                    reason: "effect_committed".to_string(),
                },
                Some(EffectState::Claimed) => RecoveryDecision {
                    disposition: RecoveryDisposition::Interrupt,
                    reason: "effect_outcome_ambiguous".to_string(),
                },
                Some(EffectState::Failed) | None if record.attempt < self.max_attempts => {
                    RecoveryDecision {
                        disposition: RecoveryDisposition::Retry,
                        reason: "tool_effect_not_committed".to_string(),
                    }
                }
                Some(EffectState::Failed) | None => RecoveryDecision {
                    disposition: RecoveryDisposition::Interrupt,
                    reason: "attempt_limit_reached".to_string(),
                },
            };
        }
        match record.phase {
            ExecutionPhase::Approval | ExecutionPhase::Question | ExecutionPhase::Subagent => {
                RecoveryDecision {
                    disposition: RecoveryDisposition::Resume,
                    reason: format!("durable_{}_state", record.phase.as_str()),
                }
            }
            ExecutionPhase::Provider
            | ExecutionPhase::Compaction
            | ExecutionPhase::Scheduling
            | ExecutionPhase::Finalize => {
                if record.attempt < self.max_attempts {
                    RecoveryDecision {
                        disposition: RecoveryDisposition::Retry,
                        reason: format!("retryable_{}_phase", record.phase.as_str()),
                    }
                } else {
                    RecoveryDecision {
                        disposition: RecoveryDisposition::Interrupt,
                        reason: "attempt_limit_reached".to_string(),
                    }
                }
            }
            ExecutionPhase::Tool => unreachable!("tool phase handled above"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DurableExecutionStore {
    root: PathBuf,
}

impl DurableExecutionStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn ledger_path(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id).join(EXECUTION_LEDGER_FILE)
    }

    pub fn create(&self, spec: NewExecution) -> SessionResult<CreateExecutionResult> {
        validate_path_component(&spec.session_id, "session_id")?;
        validate_path_component(&spec.execution_id, "execution_id")?;
        let _guard = execution_ledger_lock()
            .lock()
            .map_err(|_| "execution ledger lock poisoned")?;
        let _file_guard = self.lock_session_ledger(&spec.session_id)?;
        let projected = self.project_session_unlocked(&spec.session_id)?;
        if let Some(existing) = projected.values().find(|record| {
            record.kind == spec.kind && record.idempotency_key == spec.idempotency_key
        }) {
            return Ok(CreateExecutionResult {
                record: existing.clone(),
                deduplicated: true,
            });
        }
        if projected.contains_key(&spec.execution_id) {
            return Err(format!("execution already exists: {}", spec.execution_id).into());
        }
        let timestamp_ms = now_ms();
        let record = DurableExecutionRecord {
            schema_version: EXECUTION_RECORD_SCHEMA_VERSION.to_string(),
            execution_id: spec.execution_id,
            session_id: spec.session_id,
            kind: spec.kind,
            parent_execution_id: spec.parent_execution_id,
            status: spec.status,
            phase: spec.phase,
            attempt: spec.attempt.max(1),
            idempotency_key: spec.idempotency_key,
            created_at_ms: timestamp_ms,
            updated_at_ms: timestamp_ms,
            lease: spec.lease,
            recovery: None,
            reason: None,
            effect: None,
            metadata: spec.metadata,
        };
        self.append_record_unlocked("execution.created", record.clone())?;
        Ok(CreateExecutionResult {
            record,
            deduplicated: false,
        })
    }

    pub fn upsert_snapshot(
        &self,
        mut record: DurableExecutionRecord,
        event_type: &str,
    ) -> SessionResult<DurableExecutionRecord> {
        validate_path_component(&record.session_id, "session_id")?;
        validate_path_component(&record.execution_id, "execution_id")?;
        record.schema_version = EXECUTION_RECORD_SCHEMA_VERSION.to_string();
        record.attempt = record.attempt.max(1);
        record.updated_at_ms = now_ms();
        self.append_record(event_type, record.clone())?;
        Ok(record)
    }

    pub fn transition(
        &self,
        session_id: &str,
        execution_id: &str,
        next: ExecutionStatus,
        phase: ExecutionPhase,
        reason: Option<&str>,
    ) -> SessionResult<DurableExecutionRecord> {
        let mut record = self
            .get(session_id, execution_id)?
            .ok_or_else(|| format!("execution not found: {execution_id}"))?;
        if !record.status.can_transition_to(next) {
            return Err(format!(
                "invalid execution transition: {} -> {}",
                record.status, next
            )
            .into());
        }
        record.status = next;
        record.phase = phase;
        record.reason = reason.map(ToString::to_string);
        if next.is_terminal() {
            record.lease = None;
        }
        self.upsert_snapshot(record, "execution.transitioned")
    }

    pub fn retry(
        &self,
        session_id: &str,
        execution_id: &str,
        reason: &str,
    ) -> SessionResult<DurableExecutionRecord> {
        let mut record = self
            .get(session_id, execution_id)?
            .ok_or_else(|| format!("execution not found: {execution_id}"))?;
        if !record.status.can_transition_to(ExecutionStatus::Queued) {
            return Err(format!("execution cannot be retried from {}", record.status).into());
        }
        record.status = ExecutionStatus::Queued;
        record.phase = ExecutionPhase::Scheduling;
        record.attempt = record.attempt.saturating_add(1);
        record.reason = Some(reason.to_string());
        record.recovery = Some(RecoveryDisposition::Retry);
        record.lease = None;
        self.upsert_snapshot(record, "execution.retried")
    }

    pub fn heartbeat(
        &self,
        session_id: &str,
        execution_id: &str,
        owner_id: &str,
        ttl_ms: u64,
    ) -> SessionResult<DurableExecutionRecord> {
        let mut record = self
            .get(session_id, execution_id)?
            .ok_or_else(|| format!("execution not found: {execution_id}"))?;
        if record.status.is_terminal() {
            return Err("terminal execution cannot renew a lease".into());
        }
        let timestamp_ms = now_ms();
        let claimed_at_ms = record
            .lease
            .as_ref()
            .filter(|lease| lease.owner_id == owner_id)
            .map_or(timestamp_ms, |lease| lease.claimed_at_ms);
        record.lease = Some(ExecutionLease {
            owner_id: owner_id.to_string(),
            claimed_at_ms,
            heartbeat_at_ms: timestamp_ms,
            expires_at_ms: timestamp_ms.saturating_add(ttl_ms.max(1)),
        });
        self.upsert_snapshot(record, "execution.heartbeat")
    }

    pub fn get(
        &self,
        session_id: &str,
        execution_id: &str,
    ) -> SessionResult<Option<DurableExecutionRecord>> {
        Ok(self.project_session(session_id)?.remove(execution_id))
    }

    pub fn find_by_idempotency_key(
        &self,
        session_id: &str,
        kind: ExecutionKind,
        idempotency_key: &str,
    ) -> SessionResult<Option<DurableExecutionRecord>> {
        Ok(self
            .project_session(session_id)?
            .into_values()
            .find(|record| record.kind == kind && record.idempotency_key == idempotency_key))
    }

    pub fn read_events(&self, session_id: &str) -> SessionResult<Vec<ExecutionLifecycleEvent>> {
        validate_path_component(session_id, "session_id")?;
        let _guard = execution_ledger_lock()
            .lock()
            .map_err(|_| "execution ledger lock poisoned")?;
        let _file_guard = self.lock_session_ledger(session_id)?;
        self.read_events_unlocked(session_id)
    }

    fn read_events_unlocked(
        &self,
        session_id: &str,
    ) -> SessionResult<Vec<ExecutionLifecycleEvent>> {
        let path = self.ledger_path(session_id);
        let Some(raw) = read_optional_string(&path)? else {
            return Ok(Vec::new());
        };
        raw.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<ExecutionLifecycleEvent>(line).map_err(Into::into))
            .collect()
    }

    pub fn project_session(
        &self,
        session_id: &str,
    ) -> SessionResult<BTreeMap<String, DurableExecutionRecord>> {
        let mut records = BTreeMap::new();
        for event in self.read_events(session_id)? {
            records.insert(event.execution_id, event.record);
        }
        Ok(records)
    }

    fn project_session_unlocked(
        &self,
        session_id: &str,
    ) -> SessionResult<BTreeMap<String, DurableExecutionRecord>> {
        let mut records = BTreeMap::new();
        for event in self.read_events_unlocked(session_id)? {
            records.insert(event.execution_id, event.record);
        }
        Ok(records)
    }

    pub fn recover_session(
        &self,
        session_id: &str,
        policy: &RecoveryPolicy,
        timestamp_ms: u64,
    ) -> SessionResult<Vec<(DurableExecutionRecord, RecoveryDecision)>> {
        let mut decisions = Vec::new();
        for mut record in self.project_session(session_id)?.into_values() {
            let decision = policy.classify(&record, timestamp_ms);
            record.recovery = Some(decision.disposition);
            record.reason = Some(decision.reason.clone());
            match decision.disposition {
                RecoveryDisposition::Retry => {
                    record.status = ExecutionStatus::Queued;
                    record.phase = ExecutionPhase::Scheduling;
                    record.attempt = record.attempt.saturating_add(1);
                    record.lease = None;
                }
                RecoveryDisposition::Interrupt => {
                    record.status = ExecutionStatus::Interrupted;
                    record.lease = None;
                }
                RecoveryDisposition::Resume | RecoveryDisposition::Ignore => {}
            }
            if decision.disposition != RecoveryDisposition::Ignore {
                record = self.upsert_snapshot(record, "execution.recovered")?;
            }
            decisions.push((record, decision));
        }
        Ok(decisions)
    }

    pub fn claim_effect(
        &self,
        session_id: &str,
        execution_id: &str,
        idempotency_key: &str,
        phase: ExecutionPhase,
    ) -> SessionResult<EffectClaim> {
        let mut record = self
            .get(session_id, execution_id)?
            .ok_or_else(|| format!("execution not found: {execution_id}"))?;
        let path = self.effect_path(idempotency_key);
        let timestamp_ms = now_ms();
        let receipt = EffectReceipt {
            schema_version: EFFECT_RECEIPT_SCHEMA_VERSION.to_string(),
            idempotency_key: idempotency_key.to_string(),
            execution_id: execution_id.to_string(),
            phase,
            state: EffectState::Claimed,
            claimed_at_ms: timestamp_ms,
            updated_at_ms: timestamp_ms,
            result: None,
        };
        match create_json_file(&path, &receipt) {
            Ok(()) => {
                record.phase = phase;
                record.effect = Some(receipt.clone());
                self.upsert_snapshot(record, "effect.claimed")?;
                Ok(EffectClaim::Acquired(receipt))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing: EffectReceipt =
                    serde_json::from_str(&fs::read_to_string(&path).map_err(|read_error| {
                        format!(
                            "failed to read effect receipt {}: {read_error}",
                            path.display()
                        )
                    })?)?;
                if existing.state == EffectState::Committed {
                    Ok(EffectClaim::AlreadyCommitted(existing))
                } else {
                    Ok(EffectClaim::Uncertain(existing))
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn commit_effect(
        &self,
        session_id: &str,
        execution_id: &str,
        idempotency_key: &str,
        result: Option<Value>,
    ) -> SessionResult<EffectReceipt> {
        let path = self.effect_path(idempotency_key);
        let mut receipt: EffectReceipt = serde_json::from_str(&fs::read_to_string(&path)?)?;
        if receipt.execution_id != execution_id {
            return Err("effect receipt belongs to another execution".into());
        }
        receipt.state = EffectState::Committed;
        receipt.updated_at_ms = now_ms();
        receipt.result = result;
        write_json_atomic(&path, &receipt)?;
        let mut record = self
            .get(session_id, execution_id)?
            .ok_or_else(|| format!("execution not found: {execution_id}"))?;
        record.effect = Some(receipt.clone());
        self.upsert_snapshot(record, "effect.committed")?;
        Ok(receipt)
    }

    fn append_record(
        &self,
        event_type: &str,
        record: DurableExecutionRecord,
    ) -> SessionResult<ExecutionLifecycleEvent> {
        let _guard = execution_ledger_lock()
            .lock()
            .map_err(|_| "execution ledger lock poisoned")?;
        let _file_guard = self.lock_session_ledger(&record.session_id)?;
        self.append_record_unlocked(event_type, record)
    }

    fn append_record_unlocked(
        &self,
        event_type: &str,
        record: DurableExecutionRecord,
    ) -> SessionResult<ExecutionLifecycleEvent> {
        let path = self.ledger_path(&record.session_id);
        let seq = next_event_seq(&path)?;
        let timestamp_ms = record.updated_at_ms;
        let event = ExecutionLifecycleEvent {
            schema_version: EXECUTION_LEDGER_SCHEMA_VERSION.to_string(),
            event_id: format!(
                "exec_evt:{}:{}:{}",
                record.session_id, record.execution_id, seq
            ),
            seq,
            event_type: event_type.to_string(),
            timestamp_ms,
            session_id: record.session_id.clone(),
            execution_id: record.execution_id.clone(),
            record,
        };
        append_json_line(&path, &event)?;
        if let Ok(catalog) = DurableSessionCatalog::open(&self.root) {
            let _ = catalog.apply_execution_event(&event);
        }
        Ok(event)
    }

    fn effect_path(&self, idempotency_key: &str) -> PathBuf {
        let digest = Sha256::digest(idempotency_key.as_bytes());
        self.root
            .join(RUNTIME_STATE_DIR)
            .join(EFFECT_RECEIPT_DIR)
            .join(format!("{digest:x}.json"))
    }

    fn lock_session_ledger(&self, session_id: &str) -> SessionResult<LedgerFileLock> {
        let session_dir = self.root.join(session_id);
        fs::create_dir_all(&session_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(session_dir.join(EXECUTION_LEDGER_LOCK_FILE))?;
        FileExt::lock_exclusive(&file)?;
        Ok(LedgerFileLock { _file: file })
    }
}

struct LedgerFileLock {
    _file: fs::File,
}

fn execution_ledger_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CatalogSession {
    pub session_id: String,
    pub workspace: String,
    pub status: String,
    pub title: String,
    pub parent_session_id: Option<String>,
    pub updated_at_ms: u64,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CatalogMessageHit {
    pub session_id: String,
    pub message_id: String,
    pub role: String,
    pub content: String,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogRebuildReport {
    pub session_count: u64,
    pub execution_count: u64,
    pub lifecycle_event_count: u64,
    pub message_count: u64,
}

#[derive(Clone, Debug)]
pub struct DurableSessionCatalog {
    path: PathBuf,
}

impl DurableSessionCatalog {
    pub fn open(root: impl AsRef<Path>) -> SessionResult<Self> {
        let path = root
            .as_ref()
            .join(RUNTIME_STATE_DIR)
            .join(SESSION_CATALOG_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let catalog = Self { path };
        catalog.initialize()?;
        Ok(catalog)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rebuild(&self, root: impl AsRef<Path>) -> SessionResult<CatalogRebuildReport> {
        let root = root.as_ref();
        let execution_store = DurableExecutionStore::new(root);
        let session_store = FileSessionStore::new(root);
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "
            DELETE FROM lifecycle_events;
            DELETE FROM executions;
            DELETE FROM sessions;
            DELETE FROM messages;
            DELETE FROM message_fts;
            ",
        )?;
        let mut report = CatalogRebuildReport::default();
        let mut entries = fs::read_dir(root)?
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let session_id = entry.file_name().to_string_lossy().to_string();
            let state = read_json_value(&entry.path().join("state.latest.json"))?;
            if state.as_object().is_none_or(Map::is_empty) {
                continue;
            }
            insert_session_projection(&transaction, &session_id, &state)?;
            report.session_count = report.session_count.saturating_add(1);
            for event in execution_store.read_events(&session_id)? {
                insert_execution_event(&transaction, &event)?;
                report.lifecycle_event_count = report.lifecycle_event_count.saturating_add(1);
            }
            report.execution_count = report
                .execution_count
                .saturating_add(execution_store.project_session(&session_id)?.len() as u64);
            for message in session_store.list_messages_with_parts(&session_id, None, None)? {
                let content = message
                    .parts
                    .iter()
                    .filter_map(|part| value_text(&part.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                if content.is_empty() {
                    continue;
                }
                let role = serde_json::to_value(&message.info.role)?
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                insert_message_projection(
                    &transaction,
                    &session_id,
                    &message.info.id,
                    &role,
                    &content,
                    message.info.created_at_ms,
                )?;
                report.message_count = report.message_count.saturating_add(1);
            }
        }
        transaction.execute(
            "INSERT INTO catalog_meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![SESSION_CATALOG_SCHEMA_VERSION.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO catalog_meta(key, value) VALUES('rebuilt_at_ms', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![now_ms().to_string()],
        )?;
        transaction.commit()?;
        Ok(report)
    }

    pub fn list_sessions(
        &self,
        query: Option<&str>,
        limit: usize,
    ) -> SessionResult<Vec<CatalogSession>> {
        let connection = self.connection()?;
        let query = query.unwrap_or_default().trim();
        let like_query = format!("%{query}%");
        let mut statement = connection.prepare(
            "SELECT session_id, workspace, status, title, parent_session_id, updated_at_ms, metadata_json
             FROM sessions
             WHERE ?1 = '' OR title LIKE ?2 OR workspace LIKE ?2 OR metadata_json LIKE ?2
             ORDER BY updated_at_ms DESC, session_id ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(params![query, like_query, limit.max(1) as u64], |row| {
            let metadata_raw: String = row.get(6)?;
            Ok(CatalogSession {
                session_id: row.get(0)?,
                workspace: row.get(1)?,
                status: row.get(2)?,
                title: row.get(3)?,
                parent_session_id: row.get(4)?,
                updated_at_ms: row.get(5)?,
                metadata: serde_json::from_str(&metadata_raw).unwrap_or_else(|_| json!({})),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_executions(&self, session_id: &str) -> SessionResult<Vec<DurableExecutionRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT record_json FROM executions
             WHERE session_id = ?1
             ORDER BY updated_at_ms DESC, execution_id ASC",
        )?;
        let rows = statement.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let raw = row?;
            serde_json::from_str::<DurableExecutionRecord>(&raw).map_err(Into::into)
        })
        .collect()
    }

    pub fn execution(&self, execution_id: &str) -> SessionResult<Option<DurableExecutionRecord>> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT record_json FROM executions WHERE execution_id = ?1",
                params![execution_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn search_messages(
        &self,
        query: &str,
        limit: usize,
    ) -> SessionResult<Vec<CatalogMessageHit>> {
        let connection = self.connection()?;
        let phrase = format!("\"{}\"", query.trim().replace('"', "\"\""));
        let mut statement = connection.prepare(
            "SELECT messages.session_id, messages.message_id, messages.role,
                    messages.content, messages.created_at_ms
             FROM message_fts
             JOIN messages ON messages.session_id = message_fts.session_id
                          AND messages.message_id = message_fts.message_id
             WHERE message_fts MATCH ?1
             ORDER BY messages.created_at_ms DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![phrase, limit.max(1) as u64], |row| {
            Ok(CatalogMessageHit {
                session_id: row.get(0)?,
                message_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn counts(&self) -> SessionResult<Value> {
        let connection = self.connection()?;
        let sessions: u64 =
            connection.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        let executions: u64 =
            connection.query_row("SELECT COUNT(*) FROM executions", [], |row| row.get(0))?;
        let events: u64 =
            connection.query_row("SELECT COUNT(*) FROM lifecycle_events", [], |row| {
                row.get(0)
            })?;
        let messages: u64 =
            connection.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
        Ok(json!({
            "schema_version": SESSION_CATALOG_SCHEMA_VERSION,
            "sessions": sessions,
            "executions": executions,
            "lifecycle_events": events,
            "messages": messages,
            "path": self.path.to_string_lossy(),
            "source_of_truth": "append_only_ledgers",
            "rebuildable": true,
        }))
    }

    fn apply_execution_event(&self, event: &ExecutionLifecycleEvent) -> SessionResult<()> {
        let connection = self.connection()?;
        insert_execution_event(&connection, event)
    }

    fn initialize(&self) -> SessionResult<()> {
        self.connection()?.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            CREATE TABLE IF NOT EXISTS catalog_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                workspace TEXT NOT NULL,
                status TEXT NOT NULL,
                title TEXT NOT NULL,
                parent_session_id TEXT,
                updated_at_ms INTEGER NOT NULL,
                metadata_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS executions (
                execution_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                parent_execution_id TEXT,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                phase TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                idempotency_key TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                lease_owner TEXT,
                lease_expires_at_ms INTEGER,
                recovery TEXT,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS executions_session_status
                ON executions(session_id, status, updated_at_ms);
            CREATE INDEX IF NOT EXISTS executions_parent
                ON executions(parent_execution_id, updated_at_ms);
            CREATE UNIQUE INDEX IF NOT EXISTS executions_idempotency
                ON executions(session_id, kind, idempotency_key);
            CREATE TABLE IF NOT EXISTS lifecycle_events (
                event_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                execution_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                status TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                payload_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS lifecycle_events_session_seq
                ON lifecycle_events(session_id, seq);
            CREATE TABLE IF NOT EXISTS messages (
                session_id TEXT NOT NULL,
                message_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY(session_id, message_id)
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
                session_id UNINDEXED,
                message_id UNINDEXED,
                content
            );
            ",
        )?;
        Ok(())
    }

    fn connection(&self) -> SessionResult<Connection> {
        Ok(Connection::open(&self.path)?)
    }
}

fn insert_session_projection(
    connection: &Connection,
    fallback_session_id: &str,
    state: &Value,
) -> SessionResult<()> {
    let metadata = state.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let session_id = state
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or(fallback_session_id);
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parent_session_id = metadata.get("parent_session_id").and_then(Value::as_str);
    connection.execute(
        "INSERT INTO sessions(
            session_id, workspace, status, title, parent_session_id, updated_at_ms, metadata_json
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(session_id) DO UPDATE SET
            workspace=excluded.workspace,
            status=excluded.status,
            title=excluded.title,
            parent_session_id=excluded.parent_session_id,
            updated_at_ms=excluded.updated_at_ms,
            metadata_json=excluded.metadata_json",
        params![
            session_id,
            state
                .get("workspace")
                .and_then(Value::as_str)
                .unwrap_or("."),
            state
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("idle"),
            title,
            parent_session_id,
            state
                .get("updated_at_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            serde_json::to_string(&metadata)?,
        ],
    )?;
    Ok(())
}

fn insert_execution_event(
    connection: &Connection,
    event: &ExecutionLifecycleEvent,
) -> SessionResult<()> {
    let record = &event.record;
    connection.execute(
        "INSERT OR IGNORE INTO lifecycle_events(
            event_id, session_id, execution_id, seq, event_type, status, timestamp_ms, payload_json
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            event.event_id,
            event.session_id,
            event.execution_id,
            event.seq,
            event.event_type,
            record.status.as_str(),
            event.timestamp_ms,
            serde_json::to_string(event)?,
        ],
    )?;
    connection.execute(
        "INSERT INTO executions(
            execution_id, session_id, parent_execution_id, kind, status, phase, attempt,
            idempotency_key, updated_at_ms, lease_owner, lease_expires_at_ms, recovery, record_json
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(execution_id) DO UPDATE SET
            session_id=excluded.session_id,
            parent_execution_id=excluded.parent_execution_id,
            kind=excluded.kind,
            status=excluded.status,
            phase=excluded.phase,
            attempt=excluded.attempt,
            idempotency_key=excluded.idempotency_key,
            updated_at_ms=excluded.updated_at_ms,
            lease_owner=excluded.lease_owner,
            lease_expires_at_ms=excluded.lease_expires_at_ms,
            recovery=excluded.recovery,
            record_json=excluded.record_json",
        params![
            record.execution_id,
            record.session_id,
            record.parent_execution_id,
            record.kind.as_str(),
            record.status.as_str(),
            record.phase.as_str(),
            record.attempt,
            record.idempotency_key,
            record.updated_at_ms,
            record.lease.as_ref().map(|lease| lease.owner_id.as_str()),
            record.lease.as_ref().map(|lease| lease.expires_at_ms),
            record.recovery.map(RecoveryDisposition::as_str),
            serde_json::to_string(record)?,
        ],
    )?;
    Ok(())
}

fn insert_message_projection(
    connection: &Connection,
    session_id: &str,
    message_id: &str,
    role: &str,
    content: &str,
    created_at_ms: u64,
) -> SessionResult<()> {
    connection.execute(
        "INSERT INTO messages(session_id, message_id, role, content, created_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![session_id, message_id, role, content, created_at_ms],
    )?;
    connection.execute(
        "INSERT INTO message_fts(session_id, message_id, content) VALUES(?1, ?2, ?3)",
        params![session_id, message_id, content],
    )?;
    Ok(())
}

fn value_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return (!text.is_empty()).then(|| text.to_string());
    }
    for key in ["text", "content", "output", "summary"] {
        if let Some(text) = value.get(key).and_then(Value::as_str)
            && !text.is_empty()
        {
            return Some(text.to_string());
        }
    }
    None
}

fn validate_path_component(value: &str, label: &str) -> SessionResult<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(format!("invalid {label}: {value}").into());
    }
    Ok(())
}

fn next_event_seq(path: &Path) -> SessionResult<u64> {
    let Some(raw) = read_optional_string(path)? else {
        return Ok(1);
    };
    Ok(raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| value.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or_default()
        .saturating_add(1))
}

fn append_json_line(path: &Path, value: &impl Serialize) -> SessionResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    Ok(())
}

fn create_json_file(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_data()
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> SessionResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_optional_string(path: &Path) -> SessionResult<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_json_value(path: &Path) -> SessionResult<Value> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(error.into()),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

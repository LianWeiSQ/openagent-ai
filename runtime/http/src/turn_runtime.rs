use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug)]
pub(super) struct TurnJobEntry {
    runtime_root: PathBuf,
    session_id: String,
    turn_id: String,
    status: ExecutionStatus,
    phase: ExecutionPhase,
    attempt: u32,
    idempotency_key: String,
    recovery: Option<RecoveryDisposition>,
    terminal_reason: Option<String>,
    lease: Option<ExecutionLease>,
    started_at_ms: u64,
    updated_at_ms: u64,
    cancel_requested_at_ms: Option<u64>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct TurnJobSnapshot {
    session_id: String,
    pub(super) turn_id: String,
    status: ExecutionStatus,
    #[serde(default)]
    phase: ExecutionPhase,
    #[serde(default = "default_turn_attempt")]
    attempt: u32,
    #[serde(default)]
    idempotency_key: String,
    #[serde(default)]
    recovery: Option<RecoveryDisposition>,
    #[serde(default)]
    terminal_reason: Option<String>,
    #[serde(default)]
    lease: Option<ExecutionLease>,
    started_at_ms: u64,
    updated_at_ms: u64,
    cancel_requested: bool,
    cancel_requested_at_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) struct QueuedTurnJob {
    runtime_root: PathBuf,
    session_id: String,
    turn_id: String,
    queued_at_ms: u64,
    payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct QueuedTurnPayload {
    schema_version: u64,
    session_id: String,
    turn_id: String,
    queued_at_ms: u64,
    payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct QueuedTurnLease {
    schema_version: u64,
    owner_id: String,
    turn_id: String,
    claimed_at_ms: u64,
    #[serde(default)]
    heartbeat_at_ms: u64,
    updated_at_ms: u64,
    #[serde(default)]
    expires_at_ms: u64,
}

pub(super) enum TurnJobRegisterError {
    Unavailable,
    QueuePersistFailed(String),
    QueueFull {
        queued_count: usize,
        max_queued_turns_per_session: usize,
    },
}

pub(super) enum TurnJobRegistration {
    Running(Arc<AtomicBool>),
    Existing {
        job: Value,
    },
    Queued {
        job: TurnJobSnapshot,
        queue_position: usize,
        queue_reason: &'static str,
    },
}

impl TurnJobEntry {
    fn to_snapshot(&self) -> TurnJobSnapshot {
        TurnJobSnapshot {
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            status: self.status,
            phase: self.phase,
            attempt: self.attempt,
            idempotency_key: self.idempotency_key.clone(),
            recovery: self.recovery,
            terminal_reason: self.terminal_reason.clone(),
            lease: self.lease.clone(),
            started_at_ms: self.started_at_ms,
            updated_at_ms: self.updated_at_ms,
            cancel_requested: self.cancel.load(Ordering::SeqCst),
            cancel_requested_at_ms: self.cancel_requested_at_ms,
        }
    }

    fn to_value(&self) -> Value {
        self.to_snapshot().to_value()
    }
}

impl TurnJobSnapshot {
    pub(super) fn to_value(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "turn_id": self.turn_id,
            "status": self.status,
            "phase": self.phase,
            "attempt": self.attempt,
            "idempotency_key": self.idempotency_key,
            "recovery": self.recovery,
            "terminal_reason": self.terminal_reason,
            "lease": self.lease,
            "started_at_ms": self.started_at_ms,
            "updated_at_ms": self.updated_at_ms,
            "cancel_requested": self.cancel_requested,
            "cancel_requested_at_ms": self.cancel_requested_at_ms,
        })
    }
}

pub(super) fn turn_job_snapshot_from_execution(record: &DurableExecutionRecord) -> TurnJobSnapshot {
    TurnJobSnapshot {
        session_id: record.session_id.clone(),
        turn_id: record.execution_id.clone(),
        status: record.status,
        phase: record.phase,
        attempt: record.attempt,
        idempotency_key: record.idempotency_key.clone(),
        recovery: record.recovery,
        terminal_reason: record.reason.clone(),
        lease: record.lease.clone(),
        started_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        cancel_requested: false,
        cancel_requested_at_ms: None,
    }
}

pub(super) fn turn_execution_record(snapshot: &TurnJobSnapshot) -> DurableExecutionRecord {
    DurableExecutionRecord {
        schema_version: "openagent.execution.v1".to_string(),
        execution_id: snapshot.turn_id.clone(),
        session_id: snapshot.session_id.clone(),
        kind: ExecutionKind::Turn,
        parent_execution_id: None,
        status: snapshot.status,
        phase: snapshot.phase,
        attempt: snapshot.attempt.max(1),
        idempotency_key: if snapshot.idempotency_key.is_empty() {
            snapshot.turn_id.clone()
        } else {
            snapshot.idempotency_key.clone()
        },
        created_at_ms: snapshot.started_at_ms,
        updated_at_ms: snapshot.updated_at_ms,
        lease: snapshot.lease.clone(),
        recovery: snapshot.recovery,
        reason: snapshot.terminal_reason.clone(),
        effect: None,
        metadata: BTreeMap::from([
            (
                "cancel_requested".to_string(),
                json!(snapshot.cancel_requested),
            ),
            (
                "cancel_requested_at_ms".to_string(),
                json!(snapshot.cancel_requested_at_ms),
            ),
        ]),
    }
}

const fn default_turn_attempt() -> u32 {
    1
}

pub(super) fn turn_jobs() -> &'static Mutex<BTreeMap<String, TurnJobEntry>> {
    static TURN_JOBS: OnceLock<Mutex<BTreeMap<String, TurnJobEntry>>> = OnceLock::new();
    TURN_JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn turn_job_index_lock() -> &'static Mutex<()> {
    static TURN_JOB_INDEX_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TURN_JOB_INDEX_LOCK.get_or_init(|| Mutex::new(()))
}

pub(super) fn turn_scheduler_lock() -> &'static Mutex<()> {
    static TURN_SCHEDULER_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TURN_SCHEDULER_LOCK.get_or_init(|| Mutex::new(()))
}

pub(super) fn queued_turns() -> &'static Mutex<BTreeMap<String, VecDeque<QueuedTurnJob>>> {
    static QUEUED_TURNS: OnceLock<Mutex<BTreeMap<String, VecDeque<QueuedTurnJob>>>> =
        OnceLock::new();
    QUEUED_TURNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn turn_queue_key(root: &Path, session_id: &str) -> String {
    format!("{}\0{session_id}", root.to_string_lossy())
}

pub(super) fn session_has_running_turn(
    jobs: &BTreeMap<String, TurnJobEntry>,
    root: &Path,
    session_id: &str,
) -> bool {
    jobs.values().any(|job| {
        job.runtime_root == root
            && job.session_id == session_id
            && job.status != ExecutionStatus::Queued
            && !job.status.is_terminal()
    })
}

pub(super) fn running_turn_worker_count(
    jobs: &BTreeMap<String, TurnJobEntry>,
    root: &Path,
) -> usize {
    jobs.values()
        .filter(|job| {
            job.runtime_root == root
                && job.status != ExecutionStatus::Queued
                && !job.status.is_terminal()
        })
        .count()
}

pub(super) fn max_running_turn_workers(config: &HttpRuntimeConfig) -> usize {
    config.max_running_turn_workers.max(1)
}

pub(super) fn turn_queue_timeout_ms(config: &HttpRuntimeConfig) -> u64 {
    config.turn_queue_timeout_ms.max(1)
}

pub(super) fn queued_turn_expired(config: &HttpRuntimeConfig, queued_at_ms: u64, now: u64) -> bool {
    now.saturating_sub(queued_at_ms) >= turn_queue_timeout_ms(config)
}

pub(super) fn turn_request_idempotency_key(payload: &Value) -> Option<String> {
    payload
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn turn_idempotency_key(payload: &Value, turn_id: &str) -> String {
    let base = turn_request_idempotency_key(payload).unwrap_or_else(|| turn_id.to_string());
    let retry_count = payload
        .get(INTERNAL_TURN_RETRY_KEY)
        .and_then(|value| value.get("retry_count"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if retry_count == 0 {
        base
    } else {
        format!("{base}:retry:{retry_count}")
    }
}

pub(super) fn turn_attempt(payload: &Value) -> u32 {
    payload
        .get(INTERNAL_TURN_RETRY_KEY)
        .and_then(|value| value.get("retry_count"))
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .saturating_add(1)
        .min(u64::from(u32::MAX)) as u32
}

pub(super) fn register_turn_job(
    config: &HttpRuntimeConfig,
    session_id: &str,
    turn_id: &str,
    payload: Value,
) -> Result<TurnJobRegistration, TurnJobRegisterError> {
    let now = now_ms();
    let root = session_root(config);
    let cancel = Arc::new(AtomicBool::new(false));
    let _scheduler_guard = turn_scheduler_lock()
        .lock()
        .map_err(|_| TurnJobRegisterError::Unavailable)?;
    let (queue, queue_reason) = {
        let jobs = turn_jobs()
            .lock()
            .map_err(|_| TurnJobRegisterError::Unavailable)?;
        let session_active = session_has_running_turn(&jobs, &root, session_id);
        let global_quota_full =
            running_turn_worker_count(&jobs, &root) >= max_running_turn_workers(config);
        (
            session_active || global_quota_full,
            if session_active {
                "session_active"
            } else if global_quota_full {
                "global_worker_quota"
            } else {
                "ready"
            },
        )
    };
    let queue_position = if queue {
        let queues = queued_turns()
            .lock()
            .map_err(|_| TurnJobRegisterError::Unavailable)?;
        let queued_count = queues
            .get(&turn_queue_key(&root, session_id))
            .map(VecDeque::len)
            .unwrap_or_default();
        if queued_count >= config.max_queued_turns_per_session {
            return Err(TurnJobRegisterError::QueueFull {
                queued_count,
                max_queued_turns_per_session: config.max_queued_turns_per_session,
            });
        }
        Some(queued_count + 1)
    } else {
        None
    };
    let status = if queue {
        ExecutionStatus::Queued
    } else {
        ExecutionStatus::Running
    };
    let phase = if queue {
        ExecutionPhase::Scheduling
    } else {
        ExecutionPhase::Provider
    };
    let attempt = turn_attempt(&payload);
    let idempotency_key = turn_idempotency_key(&payload, turn_id);
    let lease = Some(ExecutionLease::new(
        runtime_owner_id(),
        now,
        config.turn_queue_lease_stale_ms,
    ));
    let mut durable_spec = NewExecution::turn(
        session_id.to_string(),
        turn_id.to_string(),
        &idempotency_key,
    );
    durable_spec.status = status;
    durable_spec.phase = phase;
    durable_spec.attempt = attempt;
    durable_spec.lease = lease.clone();
    let durable_store = DurableExecutionStore::new(session_root(config));
    let durable = durable_store
        .create(durable_spec)
        .map_err(|error| TurnJobRegisterError::QueuePersistFailed(error.to_string()))?;
    if durable.deduplicated {
        return Ok(TurnJobRegistration::Existing {
            job: turn_job_snapshot_from_execution(&durable.record).to_value(),
        });
    }
    let queued_job = if queue {
        Some(QueuedTurnJob {
            runtime_root: root.clone(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            queued_at_ms: now,
            payload: payload.clone(),
        })
    } else {
        None
    };
    if let Some(queued) = queued_job.as_ref() {
        let lease_claimed = claim_queued_turn_lease(config, &root, &queued.turn_id)
            .map_err(TurnJobRegisterError::QueuePersistFailed)?;
        if !lease_claimed {
            let _ = durable_store.transition(
                session_id,
                turn_id,
                ExecutionStatus::Failed,
                ExecutionPhase::Scheduling,
                Some("turn queue lease unavailable"),
            );
            return Err(TurnJobRegisterError::QueuePersistFailed(
                "turn queue lease unavailable".to_string(),
            ));
        }
        if let Err(error) = persist_queued_turn_payload(&root, queued, now) {
            release_queued_turn_lease(&root, &queued.turn_id);
            let _ = durable_store.transition(
                session_id,
                turn_id,
                ExecutionStatus::Failed,
                ExecutionPhase::Scheduling,
                Some(&error),
            );
            return Err(TurnJobRegisterError::QueuePersistFailed(error));
        }
    }
    let entry = TurnJobEntry {
        runtime_root: root.clone(),
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        status,
        phase,
        attempt,
        idempotency_key,
        recovery: None,
        terminal_reason: None,
        lease,
        started_at_ms: now,
        updated_at_ms: now,
        cancel_requested_at_ms: None,
        cancel: Arc::clone(&cancel),
    };
    let snapshot = entry.to_snapshot();
    {
        let mut jobs = turn_jobs()
            .lock()
            .map_err(|_| TurnJobRegisterError::Unavailable)?;
        jobs.insert(turn_id.to_string(), entry);
    }
    if queue {
        let mut queues = queued_turns()
            .lock()
            .map_err(|_| TurnJobRegisterError::Unavailable)?;
        queues
            .entry(turn_queue_key(&root, session_id))
            .or_default()
            .push_back(queued_job.expect("queued job"));
    }
    persist_turn_job_snapshot(&root, snapshot);
    if queue {
        Ok(TurnJobRegistration::Queued {
            job: turn_job_payload(turn_id)
                .and_then(|value| turn_job_snapshot_from_value(&value))
                .unwrap_or(TurnJobSnapshot {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.to_string(),
                    status: ExecutionStatus::Queued,
                    phase: ExecutionPhase::Scheduling,
                    attempt,
                    idempotency_key: turn_idempotency_key(&payload, turn_id),
                    recovery: None,
                    terminal_reason: None,
                    lease: Some(ExecutionLease::new(
                        runtime_owner_id(),
                        now,
                        config.turn_queue_lease_stale_ms,
                    )),
                    started_at_ms: now,
                    updated_at_ms: now,
                    cancel_requested: false,
                    cancel_requested_at_ms: None,
                }),
            queue_position: queue_position.unwrap_or(1),
            queue_reason,
        })
    } else {
        Ok(TurnJobRegistration::Running(cancel))
    }
}

pub(super) fn turn_job_payload(turn_id: &str) -> Option<Value> {
    turn_jobs()
        .lock()
        .ok()
        .and_then(|jobs| jobs.get(turn_id).map(TurnJobEntry::to_value))
}

pub(super) fn queued_turn_positions() -> BTreeMap<String, usize> {
    let Ok(queues) = queued_turns().lock() else {
        return BTreeMap::new();
    };
    let mut positions = BTreeMap::new();
    for queue in queues.values() {
        for (index, job) in queue.iter().enumerate() {
            positions.insert(job.turn_id.clone(), index + 1);
        }
    }
    positions
}

pub(super) fn turn_queue_dir(root: &Path) -> PathBuf {
    root.join(TURN_QUEUE_DIR)
}

pub(super) fn queued_turn_payload_path(root: &Path, turn_id: &str) -> PathBuf {
    turn_queue_dir(root).join(format!("{turn_id}.json"))
}

pub(super) fn turn_queue_lease_dir(root: &Path) -> PathBuf {
    root.join(TURN_QUEUE_LEASE_DIR)
}

pub(super) fn queued_turn_lease_path(root: &Path, turn_id: &str) -> PathBuf {
    turn_queue_lease_dir(root).join(format!("{turn_id}.lease.json"))
}

pub(super) fn runtime_owner_id() -> &'static str {
    static OWNER_ID: OnceLock<String> = OnceLock::new();
    OWNER_ID
        .get_or_init(|| format!("openagent-runtime-{}-{}", std::process::id(), now_ms()))
        .as_str()
}

pub(super) fn persist_queued_turn_payload(
    root: &Path,
    queued: &QueuedTurnJob,
    queued_at_ms: u64,
) -> Result<(), String> {
    write_json_value(
        &queued_turn_payload_path(root, &queued.turn_id),
        &json!({
            "schema_version": TURN_QUEUE_PAYLOAD_SCHEMA_VERSION,
            "session_id": queued.session_id.clone(),
            "turn_id": queued.turn_id.clone(),
            "queued_at_ms": queued_at_ms,
            "payload": queued.payload.clone(),
        }),
    )
}

pub(super) fn remove_queued_turn_payload(root: &Path, turn_id: &str) {
    let _ = fs::remove_file(queued_turn_payload_path(root, turn_id));
    release_queued_turn_lease(root, turn_id);
}

pub(super) fn queued_payload_turn_ids(root: &Path) -> BTreeSet<String> {
    read_queued_turn_payloads(root)
        .into_iter()
        .map(|queued| queued.turn_id)
        .collect()
}

pub(super) fn read_queued_turn_payloads(root: &Path) -> Vec<QueuedTurnPayload> {
    let Ok(entries) = fs::read_dir(turn_queue_dir(root)) else {
        return Vec::new();
    };
    let mut payloads = entries
        .flatten()
        .filter_map(|entry| {
            fs::read_to_string(entry.path())
                .ok()
                .and_then(|raw| serde_json::from_str::<QueuedTurnPayload>(&raw).ok())
        })
        .filter(|queued| queued.schema_version == TURN_QUEUE_PAYLOAD_SCHEMA_VERSION)
        .collect::<Vec<_>>();
    payloads.sort_by(|left, right| {
        left.queued_at_ms
            .cmp(&right.queued_at_ms)
            .then_with(|| left.turn_id.cmp(&right.turn_id))
    });
    payloads
}

pub(super) fn claim_queued_turn_lease(
    config: &HttpRuntimeConfig,
    root: &Path,
    turn_id: &str,
) -> Result<bool, String> {
    let path = queued_turn_lease_path(root, turn_id);
    let now = now_ms();
    let lease = QueuedTurnLease {
        schema_version: TURN_QUEUE_LEASE_SCHEMA_VERSION,
        owner_id: runtime_owner_id().to_string(),
        turn_id: turn_id.to_string(),
        claimed_at_ms: now,
        heartbeat_at_ms: now,
        updated_at_ms: now,
        expires_at_ms: now.saturating_add(config.turn_queue_lease_stale_ms),
    };
    match create_queued_turn_lease(&path, &lease) {
        Ok(()) => return Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let Some(existing) = read_queued_turn_lease(&path) else {
        let _ = fs::remove_file(&path);
        return match create_queued_turn_lease(&path, &lease) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.to_string()),
        };
    };
    if existing.owner_id == runtime_owner_id() {
        refresh_queued_turn_lease(&path, turn_id, config.turn_queue_lease_stale_ms);
        return Ok(true);
    }
    let expires_at_ms = if existing.expires_at_ms == 0 {
        existing
            .updated_at_ms
            .max(existing.heartbeat_at_ms)
            .max(existing.claimed_at_ms)
            .saturating_add(config.turn_queue_lease_stale_ms)
    } else {
        existing.expires_at_ms
    };
    if expires_at_ms > now {
        return Ok(false);
    }
    let _ = fs::remove_file(&path);
    match create_queued_turn_lease(&path, &lease) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn create_queued_turn_lease(
    path: &Path,
    lease: &QueuedTurnLease,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(
        stable_json_dumps(&json!({
            "schema_version": lease.schema_version,
            "owner_id": lease.owner_id,
            "turn_id": lease.turn_id,
            "claimed_at_ms": lease.claimed_at_ms,
            "heartbeat_at_ms": lease.heartbeat_at_ms,
            "updated_at_ms": lease.updated_at_ms,
            "expires_at_ms": lease.expires_at_ms,
        }))
        .as_bytes(),
    )
}

pub(super) fn read_queued_turn_lease(path: &Path) -> Option<QueuedTurnLease> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<QueuedTurnLease>(&raw).ok())
        .filter(|lease| lease.schema_version == TURN_QUEUE_LEASE_SCHEMA_VERSION)
}

pub(super) fn refresh_queued_turn_lease(path: &Path, turn_id: &str, ttl_ms: u64) {
    let now = now_ms();
    let claimed_at_ms = read_queued_turn_lease(path)
        .filter(|lease| lease.owner_id == runtime_owner_id())
        .map_or(now, |lease| lease.claimed_at_ms);
    let _ = write_json_value(
        path,
        &json!({
            "schema_version": TURN_QUEUE_LEASE_SCHEMA_VERSION,
            "owner_id": runtime_owner_id(),
            "turn_id": turn_id,
            "claimed_at_ms": claimed_at_ms,
            "heartbeat_at_ms": now,
            "updated_at_ms": now,
            "expires_at_ms": now.saturating_add(ttl_ms.max(1)),
        }),
    );
}

pub(super) fn heartbeat_owned_queued_turn_leases(config: &HttpRuntimeConfig) {
    let root = session_root(config);
    let turn_ids = queued_turns()
        .lock()
        .ok()
        .map(|queues| {
            queues
                .values()
                .flat_map(|queue| {
                    queue
                        .iter()
                        .filter(|queued| queued.runtime_root == root)
                        .map(|queued| queued.turn_id.clone())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for turn_id in turn_ids {
        let path = queued_turn_lease_path(&root, &turn_id);
        if read_queued_turn_lease(&path).is_some_and(|lease| lease.owner_id == runtime_owner_id()) {
            refresh_queued_turn_lease(&path, &turn_id, config.turn_queue_lease_stale_ms);
        }
    }
}

pub(super) fn spawn_turn_heartbeat(
    config: HttpRuntimeConfig,
    session_id: String,
    turn_id: String,
    stop: Arc<AtomicBool>,
) -> Option<thread::JoinHandle<()>> {
    let interval_ms = (config.turn_queue_lease_stale_ms / 3).clamp(10, 1_000);
    thread::Builder::new()
        .name(format!("openagent-turn-heartbeat-{turn_id}"))
        .spawn(move || {
            let store = DurableExecutionStore::new(session_root(&config));
            while !stop.load(Ordering::SeqCst) {
                thread::park_timeout(Duration::from_millis(interval_ms));
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let _ = store.heartbeat(
                    &session_id,
                    &turn_id,
                    runtime_owner_id(),
                    config.turn_queue_lease_stale_ms,
                );
                heartbeat_owned_queued_turn_leases(&config);
            }
        })
        .ok()
}

pub(super) fn release_queued_turn_lease(root: &Path, turn_id: &str) {
    let _ = fs::remove_file(queued_turn_lease_path(root, turn_id));
}

pub(super) fn remove_queued_turn_from_memory(turn_id: &str) {
    if let Ok(mut queues) = queued_turns().lock() {
        let empty_sessions = queues
            .iter_mut()
            .filter_map(|(session_id, queue)| {
                queue.retain(|queued| queued.turn_id != turn_id);
                if queue.is_empty() {
                    Some(session_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for session_id in empty_sessions {
            queues.remove(&session_id);
        }
    }
}

pub(super) fn mark_turn_job_status(config: &HttpRuntimeConfig, turn_id: &str, status: &str) {
    mark_turn_job_status_at_root(&session_root(config), turn_id, status);
}

pub(super) fn mark_turn_job_state(
    config: &HttpRuntimeConfig,
    turn_id: &str,
    status: ExecutionStatus,
    phase: ExecutionPhase,
    reason: Option<&str>,
) {
    let root = session_root(config);
    let mut snapshot = None;
    if let Ok(mut jobs) = turn_jobs().lock()
        && let Some(entry) = jobs.get_mut(turn_id)
    {
        if entry.status.can_transition_to(status) {
            entry.status = status;
        }
        entry.phase = phase;
        entry.terminal_reason = reason.map(ToString::to_string);
        entry.updated_at_ms = now_ms();
        if status.is_terminal() {
            entry.lease = None;
        }
        snapshot = Some(entry.to_snapshot());
    }
    if let Some(snapshot) = snapshot {
        persist_turn_job_snapshot(&root, snapshot);
        return;
    }
    if let Some(mut persisted) = read_turn_job_index(&root)
        .into_iter()
        .find(|job| job.turn_id == turn_id)
    {
        if persisted.status.can_transition_to(status) {
            persisted.status = status;
        }
        persisted.phase = phase;
        persisted.terminal_reason = reason.map(ToString::to_string);
        if status.is_terminal() {
            persisted.lease = None;
        }
        persist_turn_job_snapshot(&root, persisted);
    }
}

pub(super) fn expire_queued_turns(config: &HttpRuntimeConfig) -> usize {
    let Ok(_scheduler_guard) = turn_scheduler_lock().lock() else {
        return 0;
    };
    expire_queued_turns_locked(config, now_ms())
}

pub(super) fn expire_queued_turns_locked(config: &HttpRuntimeConfig, now: u64) -> usize {
    let root = session_root(config);
    let mut expired = Vec::new();
    if let Ok(mut queues) = queued_turns().lock() {
        let empty_sessions = queues
            .iter_mut()
            .filter_map(|(session_id, queue)| {
                let mut retained = VecDeque::new();
                while let Some(queued) = queue.pop_front() {
                    if queued.runtime_root == root
                        && queued_turn_expired(config, queued.queued_at_ms, now)
                    {
                        expired.push(queued);
                    } else {
                        retained.push_back(queued);
                    }
                }
                *queue = retained;
                if queue.is_empty() {
                    Some(session_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for session_id in empty_sessions {
            queues.remove(&session_id);
        }
    }
    if expired.is_empty() {
        return 0;
    }
    if let Ok(mut jobs) = turn_jobs().lock() {
        for queued in &expired {
            if let Some(entry) = jobs.get_mut(&queued.turn_id)
                && entry.status == ExecutionStatus::Queued
            {
                entry.status = ExecutionStatus::Cancelled;
                entry.phase = ExecutionPhase::Finalize;
                entry.terminal_reason = Some("queue_timeout".to_string());
                entry.updated_at_ms = now;
            }
        }
    }
    for queued in &expired {
        mark_turn_job_status_at_root(&root, &queued.turn_id, "expired");
        remove_queued_turn_payload(&root, &queued.turn_id);
    }
    expired.len()
}

pub(super) fn pop_next_schedulable_queued_turn(
    config: &HttpRuntimeConfig,
) -> Option<QueuedTurnJob> {
    let _scheduler_guard = turn_scheduler_lock().lock().ok()?;
    let root = session_root(config);
    loop {
        expire_queued_turns_locked(config, now_ms());
        {
            let jobs = turn_jobs().lock().ok()?;
            if running_turn_worker_count(&jobs, &root) >= max_running_turn_workers(config) {
                return None;
            }
        }
        let selected_session = {
            let jobs = turn_jobs().lock().ok()?;
            let queues = queued_turns().lock().ok()?;
            queues
                .iter()
                .filter_map(|(queue_key, queue)| {
                    let queued = queue.front()?;
                    if queued.runtime_root != root
                        || session_has_running_turn(&jobs, &root, &queued.session_id)
                    {
                        return None;
                    }
                    Some((
                        queue_key.clone(),
                        queued.queued_at_ms,
                        queued.turn_id.clone(),
                    ))
                })
                .min_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)))
                .map(|(session_id, _, _)| session_id)
        }?;
        let queued = {
            let mut queues = queued_turns().lock().ok()?;
            let queue = queues.get_mut(&selected_session)?;
            let queued = queue.pop_front();
            if queue.is_empty() {
                queues.remove(&selected_session);
            }
            queued
        }?;
        let now = now_ms();
        let mut snapshot = None;
        let mut should_start = false;
        let mut should_remove_payload = true;
        if let Ok(mut jobs) = turn_jobs().lock()
            && let Some(entry) = jobs.get_mut(&queued.turn_id)
            && entry.session_id == queued.session_id
            && entry.status == ExecutionStatus::Queued
        {
            if entry.cancel.load(Ordering::SeqCst) {
                entry.status = ExecutionStatus::Interrupted;
                entry.phase = ExecutionPhase::Finalize;
                entry.terminal_reason = Some("interrupt_requested".to_string());
                entry.updated_at_ms = now;
            } else if queued_turn_expired(config, queued.queued_at_ms, now) {
                entry.status = ExecutionStatus::Cancelled;
                entry.phase = ExecutionPhase::Finalize;
                entry.terminal_reason = Some("queue_timeout".to_string());
                entry.updated_at_ms = now;
            } else {
                entry.status = ExecutionStatus::Running;
                entry.phase = ExecutionPhase::Provider;
                entry.lease = Some(ExecutionLease::new(
                    runtime_owner_id(),
                    now,
                    config.turn_queue_lease_stale_ms,
                ));
                entry.updated_at_ms = now;
                should_start = true;
            }
            snapshot = Some(entry.to_snapshot());
        }
        if let Some(snapshot) = snapshot {
            persist_turn_job_snapshot(&root, snapshot);
        } else {
            should_remove_payload = true;
        }
        if should_remove_payload {
            remove_queued_turn_payload(&root, &queued.turn_id);
        }
        if should_start {
            return Some(queued);
        }
    }
}

pub(super) fn start_next_queued_turns(config: &HttpRuntimeConfig) {
    while let Some(queued) = pop_next_schedulable_queued_turn(config) {
        if let Err(error) = spawn_async_turn_worker(
            config,
            queued.session_id.clone(),
            queued.turn_id.clone(),
            queued.payload,
        ) {
            record_async_turn_failure(config, &queued.session_id, &queued.turn_id, &error);
            mark_turn_job_status(config, &queued.turn_id, "failed");
        }
    }
}

pub(super) fn mark_turn_job_status_at_root(root: &Path, turn_id: &str, status: &str) {
    let next_status = ExecutionStatus::from_runtime(status);
    let terminal_reason = match status {
        "expired" => Some("queue_timeout".to_string()),
        "canceled" | "cancelled" => Some("cancel_requested".to_string()),
        "interrupted" => Some("interrupt_requested".to_string()),
        _ => None,
    };
    let mut snapshot = None;
    if let Ok(mut jobs) = turn_jobs().lock()
        && let Some(entry) = jobs.get_mut(turn_id)
    {
        entry.status = next_status;
        if next_status.is_terminal() {
            entry.phase = ExecutionPhase::Finalize;
            entry.lease = None;
        }
        entry.terminal_reason = terminal_reason.clone();
        entry.updated_at_ms = now_ms();
        snapshot = Some(entry.to_snapshot());
    }
    if let Some(snapshot) = snapshot {
        persist_turn_job_snapshot(root, snapshot);
    } else {
        update_persisted_turn_job_status(root, turn_id, status);
    }
    if next_status.is_terminal() {
        remove_queued_turn_payload(root, turn_id);
        remove_queued_turn_from_memory(turn_id);
        if next_status != ExecutionStatus::Failed {
            remove_turn_retry_payload(root, turn_id);
        }
    }
}

pub(super) fn turn_retry_payload_path(root: &Path, turn_id: &str) -> PathBuf {
    root.join(TURN_RETRY_PAYLOAD_DIR)
        .join(format!("{turn_id}.json"))
}

pub(super) fn persist_turn_retry_payload(
    root: &Path,
    session_id: &str,
    turn_id: &str,
    payload: &Value,
) -> Result<(), String> {
    write_json_value(
        &turn_retry_payload_path(root, turn_id),
        &json!({
            "schema_version": TURN_RETRY_PAYLOAD_SCHEMA_VERSION,
            "session_id": session_id,
            "turn_id": turn_id,
            "created_at_ms": now_ms(),
            "payload": payload,
        }),
    )
}

pub(super) fn read_turn_retry_payload(root: &Path, turn_id: &str) -> Option<Value> {
    let value = read_json_file(&turn_retry_payload_path(root, turn_id));
    (value.get("schema_version").and_then(Value::as_u64) == Some(TURN_RETRY_PAYLOAD_SCHEMA_VERSION))
        .then_some(value)
}

pub(super) fn remove_turn_retry_payload(root: &Path, turn_id: &str) {
    let _ = fs::remove_file(turn_retry_payload_path(root, turn_id));
}

pub(super) fn request_turn_job_cancel(config: &HttpRuntimeConfig, turn_id: &str) -> Option<Value> {
    let root = session_root(config);
    let now = now_ms();
    let mut snapshot = None;
    let mut was_queued = false;
    if let Ok(mut jobs) = turn_jobs().lock()
        && let Some(entry) = jobs.get_mut(turn_id)
    {
        entry.cancel.store(true, Ordering::SeqCst);
        was_queued = entry.status == ExecutionStatus::Queued;
        entry.status = if entry.status == ExecutionStatus::Queued {
            ExecutionStatus::Interrupted
        } else {
            ExecutionStatus::Waiting
        };
        entry.phase = if was_queued {
            ExecutionPhase::Finalize
        } else {
            entry.phase
        };
        entry.terminal_reason = was_queued.then(|| "interrupt_requested".to_string());
        entry.cancel_requested_at_ms = Some(now);
        entry.updated_at_ms = now;
        snapshot = Some(entry.to_snapshot());
    }
    if let Some(snapshot) = snapshot {
        if was_queued {
            remove_queued_turn_payload(&root, turn_id);
            remove_queued_turn_from_memory(turn_id);
        }
        persist_turn_job_snapshot(&root, snapshot.clone());
        return Some(snapshot.to_value());
    }
    update_persisted_turn_job_cancel(&root, turn_id).map(|job| job.to_value())
}

pub(super) fn turn_cancel_requested(turn_id: &str) -> bool {
    turn_jobs()
        .lock()
        .ok()
        .and_then(|jobs| {
            jobs.get(turn_id)
                .map(|entry| entry.cancel.load(Ordering::SeqCst))
        })
        .unwrap_or(false)
}

pub(super) fn list_turn_jobs_payload(config: &HttpRuntimeConfig, request_path: &str) -> Value {
    let expired_count = expire_queued_turns(config);
    let session_filter = query_value(request_path, "session_id")
        .or_else(|| query_value(request_path, "session"))
        .filter(|value| !value.trim().is_empty());
    let status_filter =
        query_value(request_path, "status").filter(|value| !value.trim().is_empty());
    let active_only = query_value(request_path, "active")
        .as_deref()
        .is_some_and(truthy);
    let root = session_root(config);
    let _index_guard = turn_job_index_lock().lock().ok();
    let queued_payload_ids = queued_payload_turn_ids(&root);
    let mut memory_ids = BTreeSet::new();
    let mut jobs_by_id = BTreeMap::<String, TurnJobSnapshot>::new();
    for job in read_turn_job_index(&root) {
        jobs_by_id.insert(job.turn_id.clone(), job);
    }
    let memory_jobs = turn_jobs()
        .lock()
        .ok()
        .map(|jobs| {
            jobs.values()
                .filter(|job| job.runtime_root == root)
                .map(TurnJobEntry::to_snapshot)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for job in memory_jobs {
        memory_ids.insert(job.turn_id.clone());
        jobs_by_id.insert(job.turn_id.clone(), job);
    }
    let now = now_ms();
    let mut reconciled_ids = BTreeSet::new();
    let mut jobs = jobs_by_id
        .into_values()
        .map(|mut job| {
            let durable_wait = job.status == ExecutionStatus::Waiting
                && persisted_turn_wait_is_resumable(&root, &job);
            let recovery_decision =
                RecoveryPolicy::default().classify(&turn_execution_record(&job), now);
            let live_external_lease = recovery_decision.disposition == RecoveryDisposition::Ignore
                && recovery_decision.reason == "live_lease";
            if durable_wait && job.recovery != Some(RecoveryDisposition::Resume) {
                job.recovery = Some(RecoveryDisposition::Resume);
                job.terminal_reason = Some("durable_wait".to_string());
                job.updated_at_ms = now;
                reconciled_ids.insert(job.turn_id.clone());
            }
            if !(memory_ids.contains(&job.turn_id)
                || job.status.is_terminal()
                || job.status == ExecutionStatus::Queued
                    && queued_payload_ids.contains(&job.turn_id)
                || durable_wait
                || live_external_lease)
            {
                job.recovery = Some(recovery_decision.disposition);
                job.status = ExecutionStatus::Interrupted;
                job.terminal_reason = Some("runtime_restart".to_string());
                job.lease = None;
                job.cancel_requested = true;
                job.cancel_requested_at_ms.get_or_insert(now);
                job.updated_at_ms = now;
                reconciled_ids.insert(job.turn_id.clone());
            }
            job
        })
        .collect::<Vec<_>>();
    jobs = prune_turn_job_snapshots(jobs, now);
    for job in jobs
        .iter()
        .filter(|job| reconciled_ids.contains(&job.turn_id))
    {
        let _ = DurableExecutionStore::new(&root)
            .upsert_snapshot(turn_execution_record(job), "turn.reconciled");
    }
    write_turn_job_index(&root, &jobs);
    let global_running_turn_workers = jobs
        .iter()
        .filter(|entry| entry.status != ExecutionStatus::Queued && !entry.status.is_terminal())
        .count();
    jobs = jobs
        .into_iter()
        .filter(|entry| match session_filter.as_deref() {
            Some(session_id) => entry.session_id == session_id,
            None => true,
        })
        .filter(|entry| match status_filter.as_deref() {
            Some(status) => entry.status == ExecutionStatus::from_runtime(status),
            None => true,
        })
        .filter(|entry| !active_only || !entry.status.is_terminal())
        .collect::<Vec<_>>();
    jobs.sort_by(|left, right| {
        right
            .started_at_ms
            .cmp(&left.started_at_ms)
            .then_with(|| left.turn_id.cmp(&right.turn_id))
    });
    let running_count = jobs
        .iter()
        .filter(|entry| entry.status != ExecutionStatus::Queued && !entry.status.is_terminal())
        .count();
    let queued_count = jobs
        .iter()
        .filter(|entry| entry.status == ExecutionStatus::Queued)
        .count();
    let terminal_count = jobs
        .iter()
        .filter(|entry| entry.status.is_terminal())
        .count();
    let active_count = running_count + queued_count;
    let queue_positions = queued_turn_positions();
    json!({
        "turns": jobs
            .iter()
            .map(|job| {
                let mut value = job.to_value();
                let payload_persisted = queued_payload_ids.contains(&job.turn_id);
                if job.status == ExecutionStatus::Queued
                    && let Some(position) = queue_positions.get(&job.turn_id)
                    && let Some(object) = value.as_object_mut()
                {
                    object.insert("queue_position".to_string(), json!(position));
                }
                if job.status == ExecutionStatus::Queued
                    && let Some(object) = value.as_object_mut()
                {
                    object.insert("payload_persisted".to_string(), json!(payload_persisted));
                }
                value
            })
            .collect::<Vec<_>>(),
        "count": jobs.len(),
        "running_count": running_count,
        "queued_count": queued_count,
        "active_count": active_count,
        "terminal_count": terminal_count,
        "scheduler": {
            "max_queued_turns_per_session": config.max_queued_turns_per_session,
            "max_running_turn_workers": max_running_turn_workers(config),
            "running_turn_workers": global_running_turn_workers,
            "turn_queue_lease_stale_ms": config.turn_queue_lease_stale_ms,
            "turn_queue_timeout_ms": turn_queue_timeout_ms(config),
            "expired_queued_turns": expired_count,
        },
        "filters": {
            "session_id": session_filter,
            "status": status_filter,
            "active": active_only,
        },
        "source": "runtime_job_registry",
        "index_persisted": true,
    })
}

pub(super) fn turn_job_status_terminal(status: &str) -> bool {
    ExecutionStatus::from_runtime(status).is_terminal()
}

pub(super) fn configured_max_queued_turns_per_session() -> usize {
    std::env::var("OPENAGENT_MAX_QUEUED_TURNS_PER_SESSION")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_QUEUED_TURNS_PER_SESSION)
}

pub(super) fn configured_max_running_turn_workers() -> usize {
    std::env::var("OPENAGENT_MAX_RUNNING_TURN_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_RUNNING_TURN_WORKERS)
        .max(1)
}

pub(super) fn configured_turn_queue_lease_stale_ms() -> u64 {
    std::env::var("OPENAGENT_TURN_QUEUE_LEASE_STALE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TURN_QUEUE_LEASE_STALE_MS)
        .max(1)
}

pub(super) fn configured_turn_queue_timeout_ms() -> u64 {
    std::env::var("OPENAGENT_TURN_QUEUE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TURN_QUEUE_TIMEOUT_MS)
        .max(1)
}

pub(super) fn turn_job_index_path(root: &Path) -> PathBuf {
    root.join(TURN_JOB_INDEX_FILE)
}

pub(super) fn turn_job_snapshot_from_value(value: &Value) -> Option<TurnJobSnapshot> {
    serde_json::from_value::<TurnJobSnapshot>(value.clone()).ok()
}

pub(super) fn read_turn_job_index(root: &Path) -> Vec<TurnJobSnapshot> {
    read_json_file(&turn_job_index_path(root))
        .get("turns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(turn_job_snapshot_from_value)
        .collect()
}

pub(super) fn write_turn_job_index(root: &Path, jobs: &[TurnJobSnapshot]) {
    let payload = json!({
        "schema_version": TURN_JOB_INDEX_SCHEMA_VERSION,
        "updated_at_ms": now_ms(),
        "turns": jobs.iter().map(TurnJobSnapshot::to_value).collect::<Vec<_>>(),
    });
    let _ = write_json_value(&turn_job_index_path(root), &payload);
}

pub(super) fn prune_turn_job_snapshots(
    mut jobs: Vec<TurnJobSnapshot>,
    now: u64,
) -> Vec<TurnJobSnapshot> {
    jobs.retain(|job| {
        !job.status.is_terminal()
            || now.saturating_sub(job.updated_at_ms) <= TURN_JOB_TERMINAL_TTL_MS
    });
    jobs.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| right.started_at_ms.cmp(&left.started_at_ms))
            .then_with(|| left.turn_id.cmp(&right.turn_id))
    });
    jobs.truncate(MAX_TURN_JOB_INDEX_ENTRIES);
    jobs
}

pub(super) fn persist_turn_job_snapshot(root: &Path, job: TurnJobSnapshot) {
    let durable_store = DurableExecutionStore::new(root);
    let mut record = turn_execution_record(&job);
    if let Ok(Some(existing)) = durable_store.get(&job.session_id, &job.turn_id) {
        record.created_at_ms = existing.created_at_ms;
        record.effect = existing.effect;
        record.metadata.extend(existing.metadata);
    }
    let _ = durable_store.upsert_snapshot(record, "turn.snapshot");
    let Ok(_guard) = turn_job_index_lock().lock() else {
        return;
    };
    let mut jobs = read_turn_job_index(root);
    jobs.retain(|item| item.turn_id != job.turn_id);
    jobs.push(job);
    let jobs = prune_turn_job_snapshots(jobs, now_ms());
    write_turn_job_index(root, &jobs);
}

pub(super) fn update_persisted_turn_job_status(root: &Path, turn_id: &str, status: &str) {
    let Ok(_guard) = turn_job_index_lock().lock() else {
        return;
    };
    let mut jobs = read_turn_job_index(root);
    let now = now_ms();
    let mut changed = false;
    for job in &mut jobs {
        if job.turn_id != turn_id {
            continue;
        }
        job.status = ExecutionStatus::from_runtime(status);
        if job.status.is_terminal() {
            job.phase = ExecutionPhase::Finalize;
            job.lease = None;
        }
        job.terminal_reason = match status {
            "expired" => Some("queue_timeout".to_string()),
            "interrupted" => Some("runtime_restart".to_string()),
            _ => job.terminal_reason.clone(),
        };
        job.updated_at_ms = now;
        if status == "interrupted" {
            job.cancel_requested = true;
            job.cancel_requested_at_ms.get_or_insert(now);
        }
        let _ = DurableExecutionStore::new(root)
            .upsert_snapshot(turn_execution_record(job), "turn.status_changed");
        changed = true;
    }
    if changed {
        let jobs = prune_turn_job_snapshots(jobs, now);
        write_turn_job_index(root, &jobs);
    }
}

pub(super) fn update_persisted_turn_job_cancel(
    root: &Path,
    turn_id: &str,
) -> Option<TurnJobSnapshot> {
    let _guard = turn_job_index_lock().lock().ok()?;
    let mut jobs = read_turn_job_index(root);
    let now = now_ms();
    let mut updated = None;
    let mut was_queued = false;
    for job in &mut jobs {
        if job.turn_id != turn_id {
            continue;
        }
        was_queued = job.status == ExecutionStatus::Queued;
        job.status = if was_queued {
            ExecutionStatus::Interrupted
        } else {
            ExecutionStatus::Waiting
        };
        if was_queued {
            job.phase = ExecutionPhase::Finalize;
            job.terminal_reason = Some("interrupt_requested".to_string());
            job.lease = None;
        }
        job.cancel_requested = true;
        job.cancel_requested_at_ms = Some(now);
        job.updated_at_ms = now;
        let _ = DurableExecutionStore::new(root)
            .upsert_snapshot(turn_execution_record(job), "turn.cancel_requested");
        updated = Some(job.clone());
        break;
    }
    if updated.is_some() {
        let jobs = prune_turn_job_snapshots(jobs, now);
        write_turn_job_index(root, &jobs);
        if was_queued {
            remove_queued_turn_payload(root, turn_id);
            remove_queued_turn_from_memory(turn_id);
        }
    }
    updated
}

pub(super) fn recover_and_start_persisted_queued_turns(config: &HttpRuntimeConfig) {
    if !recover_persisted_queued_turns(config).is_empty() {
        start_next_queued_turns(config);
    }
    let _ = list_turn_jobs_payload(config, "/api/turns");
}

pub(super) fn recover_persisted_queued_turns(config: &HttpRuntimeConfig) -> Vec<String> {
    let root = session_root(config);
    let payloads = read_queued_turn_payloads(&root);
    if payloads.is_empty() {
        return Vec::new();
    }
    let Ok(_scheduler_guard) = turn_scheduler_lock().lock() else {
        return Vec::new();
    };
    let now = now_ms();
    let mut index_jobs = read_turn_job_index(&root);
    let mut index_changed = false;
    let snapshots = index_jobs
        .iter()
        .map(|job| (job.turn_id.clone(), job.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut recovered_sessions = BTreeSet::new();
    if let (Ok(mut jobs), Ok(mut queues)) = (turn_jobs().lock(), queued_turns().lock()) {
        for persisted in payloads {
            let Some(snapshot) = snapshots.get(&persisted.turn_id) else {
                remove_queued_turn_payload(&root, &persisted.turn_id);
                continue;
            };
            if snapshot.status != ExecutionStatus::Queued || snapshot.cancel_requested {
                remove_queued_turn_payload(&root, &persisted.turn_id);
                continue;
            }
            if queued_turn_expired(config, persisted.queued_at_ms, now) {
                if let Some(job) = index_jobs
                    .iter_mut()
                    .find(|job| job.turn_id == persisted.turn_id)
                {
                    job.status = ExecutionStatus::Cancelled;
                    job.phase = ExecutionPhase::Finalize;
                    job.terminal_reason = Some("queue_timeout".to_string());
                    job.lease = None;
                    job.updated_at_ms = now;
                    index_changed = true;
                }
                remove_queued_turn_payload(&root, &persisted.turn_id);
                continue;
            }
            match claim_queued_turn_lease(config, &root, &persisted.turn_id) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => continue,
            }
            if jobs.contains_key(&persisted.turn_id) {
                continue;
            }
            jobs.insert(
                persisted.turn_id.clone(),
                TurnJobEntry {
                    runtime_root: root.clone(),
                    session_id: persisted.session_id.clone(),
                    turn_id: persisted.turn_id.clone(),
                    status: ExecutionStatus::Queued,
                    phase: ExecutionPhase::Scheduling,
                    attempt: snapshot.attempt,
                    idempotency_key: snapshot.idempotency_key.clone(),
                    recovery: Some(RecoveryDisposition::Resume),
                    terminal_reason: None,
                    lease: Some(ExecutionLease::new(
                        runtime_owner_id(),
                        now,
                        config.turn_queue_lease_stale_ms,
                    )),
                    started_at_ms: snapshot.started_at_ms,
                    updated_at_ms: snapshot.updated_at_ms,
                    cancel_requested_at_ms: snapshot.cancel_requested_at_ms,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
            let queue = queues
                .entry(turn_queue_key(&root, &persisted.session_id))
                .or_default();
            if !queue
                .iter()
                .any(|queued| queued.turn_id == persisted.turn_id)
            {
                queue.push_back(QueuedTurnJob {
                    runtime_root: root.clone(),
                    session_id: persisted.session_id.clone(),
                    turn_id: persisted.turn_id.clone(),
                    queued_at_ms: persisted.queued_at_ms,
                    payload: persisted.payload,
                });
            }
            recovered_sessions.insert(persisted.session_id);
        }
    }
    if !recovered_sessions.is_empty() {
        for job in &mut index_jobs {
            if recovered_sessions.contains(&job.session_id)
                && job.status != ExecutionStatus::Queued
                && !job.status.is_terminal()
            {
                let decision = RecoveryPolicy::default().classify(&turn_execution_record(job), now);
                job.status = ExecutionStatus::Interrupted;
                job.recovery = Some(decision.disposition);
                job.terminal_reason = Some("runtime_restart".to_string());
                job.lease = None;
                job.cancel_requested = true;
                job.cancel_requested_at_ms.get_or_insert(now);
                job.updated_at_ms = now;
                index_changed = true;
            }
        }
    }
    if index_changed {
        let index_jobs = prune_turn_job_snapshots(index_jobs, now);
        for job in &index_jobs {
            let _ = DurableExecutionStore::new(&root)
                .upsert_snapshot(turn_execution_record(job), "turn.recovered");
        }
        write_turn_job_index(&root, &index_jobs);
    }
    recovered_sessions.into_iter().collect()
}

pub(super) fn persisted_turn_job_payload(root: &Path, turn_id: &str) -> Option<Value> {
    let Ok(_guard) = turn_job_index_lock().lock() else {
        return None;
    };
    let payload_persisted = queued_turn_payload_path(root, turn_id).exists();
    let mut jobs = read_turn_job_index(root);
    let now = now_ms();
    let mut found = None;
    let mut changed = false;
    for job in &mut jobs {
        if job.turn_id != turn_id {
            continue;
        }
        if !(job.status.is_terminal()
            || job.status == ExecutionStatus::Queued && payload_persisted
            || job.status == ExecutionStatus::Waiting
                && persisted_turn_wait_is_resumable(root, job))
        {
            let decision = RecoveryPolicy::default().classify(&turn_execution_record(job), now);
            if decision.disposition == RecoveryDisposition::Ignore
                && decision.reason == "live_lease"
            {
                found = Some(job.to_value());
                break;
            }
            job.status = ExecutionStatus::Interrupted;
            job.recovery = Some(decision.disposition);
            job.terminal_reason = Some("runtime_restart".to_string());
            job.lease = None;
            job.cancel_requested = true;
            job.cancel_requested_at_ms.get_or_insert(now);
            job.updated_at_ms = now;
            changed = true;
        }
        let mut value = job.to_value();
        if job.status == ExecutionStatus::Queued
            && let Some(object) = value.as_object_mut()
        {
            object.insert("payload_persisted".to_string(), json!(payload_persisted));
        }
        found = Some(value);
        break;
    }
    if changed {
        let jobs = prune_turn_job_snapshots(jobs, now);
        write_turn_job_index(root, &jobs);
    }
    found
}

pub(super) fn persisted_turn_wait_is_resumable(root: &Path, job: &TurnJobSnapshot) -> bool {
    let state = read_json_file(&root.join(&job.session_id).join("state.latest.json"));
    ["pending_approval", "pending_question"]
        .iter()
        .filter_map(|key| {
            state
                .get("metadata")
                .and_then(|metadata| metadata.get(*key))
        })
        .any(|pending| {
            pending
                .get("turn_id")
                .or_else(|| pending.get("run_id"))
                .and_then(Value::as_str)
                == Some(job.turn_id.as_str())
        })
}

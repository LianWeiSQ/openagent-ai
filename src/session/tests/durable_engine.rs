use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use openagent_protocol::{ChatMessage, Role};
use openagent_session::{
    DurableExecutionStore, DurableSessionCatalog, EffectClaim, ExecutionKind, ExecutionPhase,
    ExecutionStatus, NewExecution, RecoveryDisposition, RecoveryPolicy, Session, SessionStatus,
    StartRunOptions,
};
use serde_json::json;

#[test]
fn durable_execution_enforces_seven_state_contract_and_idempotent_create() {
    let root = unique_temp_dir("openagent-durable-state");
    let store = DurableExecutionStore::new(&root);
    let created = store
        .create(NewExecution::turn(
            "session_state",
            "turn_state",
            "request-state",
        ))
        .expect("execution creates");
    assert_eq!(created.record.status, ExecutionStatus::Queued);
    assert!(!created.deduplicated);

    let duplicate = store
        .create(NewExecution::turn(
            "session_state",
            "turn_duplicate",
            "request-state",
        ))
        .expect("idempotent create resolves");
    assert!(duplicate.deduplicated);
    assert_eq!(duplicate.record.execution_id, "turn_state");

    let running = store
        .transition(
            "session_state",
            "turn_state",
            ExecutionStatus::Running,
            ExecutionPhase::Provider,
            None,
        )
        .expect("queued turn starts");
    assert_eq!(running.status, ExecutionStatus::Running);
    let waiting = store
        .transition(
            "session_state",
            "turn_state",
            ExecutionStatus::Waiting,
            ExecutionPhase::Approval,
            Some("permission_required"),
        )
        .expect("running turn waits");
    assert_eq!(waiting.status, ExecutionStatus::Waiting);
    let completed = store
        .transition(
            "session_state",
            "turn_state",
            ExecutionStatus::Completed,
            ExecutionPhase::Finalize,
            None,
        )
        .expect("waiting turn completes");
    assert!(completed.status.is_terminal());
    assert!(
        store
            .transition(
                "session_state",
                "turn_state",
                ExecutionStatus::Running,
                ExecutionPhase::Provider,
                None,
            )
            .is_err(),
        "completed executions cannot restart in place"
    );

    let statuses = [
        ExecutionStatus::Queued,
        ExecutionStatus::Running,
        ExecutionStatus::Waiting,
        ExecutionStatus::Completed,
        ExecutionStatus::Failed,
        ExecutionStatus::Cancelled,
        ExecutionStatus::Interrupted,
    ];
    assert_eq!(
        statuses.map(ExecutionStatus::as_str),
        [
            "queued",
            "running",
            "waiting",
            "completed",
            "failed",
            "cancelled",
            "interrupted",
        ]
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn recovery_policy_covers_provider_tool_approval_compaction_and_subagent_crashes() {
    let root = unique_temp_dir("openagent-durable-recovery");
    let store = DurableExecutionStore::new(&root);
    let policy = RecoveryPolicy {
        max_attempts: 3,
        lease_timeout_ms: 50,
    };
    for (execution_id, phase) in [
        ("turn_provider", ExecutionPhase::Provider),
        ("turn_tool_unclaimed", ExecutionPhase::Tool),
        ("turn_tool_ambiguous", ExecutionPhase::Tool),
        ("turn_tool_committed", ExecutionPhase::Tool),
        ("turn_approval", ExecutionPhase::Approval),
        ("turn_compaction", ExecutionPhase::Compaction),
        ("task_subagent", ExecutionPhase::Subagent),
    ] {
        let kind = if execution_id.starts_with("task_") {
            ExecutionKind::Task
        } else {
            ExecutionKind::Turn
        };
        let mut spec = NewExecution::turn(
            "session_recovery",
            execution_id,
            format!("request-{execution_id}"),
        );
        spec.kind = kind;
        spec.status = ExecutionStatus::Running;
        spec.phase = phase;
        store.create(spec).expect("execution creates");
    }

    assert!(matches!(
        store
            .claim_effect(
                "session_recovery",
                "turn_tool_ambiguous",
                "effect-ambiguous",
                ExecutionPhase::Tool,
            )
            .expect("effect claims"),
        EffectClaim::Acquired(_)
    ));
    store
        .claim_effect(
            "session_recovery",
            "turn_tool_committed",
            "effect-committed",
            ExecutionPhase::Tool,
        )
        .expect("effect claims");
    store
        .commit_effect(
            "session_recovery",
            "turn_tool_committed",
            "effect-committed",
            Some(json!({"ok": true})),
        )
        .expect("effect commits");

    let decisions = [
        ("turn_provider", RecoveryDisposition::Retry),
        ("turn_tool_unclaimed", RecoveryDisposition::Retry),
        ("turn_tool_ambiguous", RecoveryDisposition::Interrupt),
        ("turn_tool_committed", RecoveryDisposition::Resume),
        ("turn_approval", RecoveryDisposition::Resume),
        ("turn_compaction", RecoveryDisposition::Retry),
        ("task_subagent", RecoveryDisposition::Resume),
    ];
    for (execution_id, expected) in decisions {
        let record = store
            .get("session_recovery", execution_id)
            .expect("projection reads")
            .expect("record exists");
        assert_eq!(
            policy.classify(&record, u64::MAX).disposition,
            expected,
            "{execution_id}"
        );
    }

    let recovered = store
        .recover_session("session_recovery", &policy, u64::MAX)
        .expect("session recovers");
    assert_eq!(recovered.len(), decisions.len());
    let ambiguous = store
        .get("session_recovery", "turn_tool_ambiguous")
        .expect("projection reads")
        .expect("record exists");
    assert_eq!(ambiguous.status, ExecutionStatus::Interrupted);
    let provider = store
        .get("session_recovery", "turn_provider")
        .expect("projection reads")
        .expect("record exists");
    assert_eq!(provider.status, ExecutionStatus::Queued);
    assert_eq!(provider.attempt, 2);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn effect_receipts_prevent_duplicate_side_effects_after_crash() {
    let root = unique_temp_dir("openagent-durable-effect");
    let store = DurableExecutionStore::new(&root);
    let mut spec = NewExecution::turn("session_effect", "turn_effect", "request-effect");
    spec.status = ExecutionStatus::Running;
    spec.phase = ExecutionPhase::Tool;
    store.create(spec).expect("execution creates");

    let first = store
        .claim_effect(
            "session_effect",
            "turn_effect",
            "write:workspace/config",
            ExecutionPhase::Tool,
        )
        .expect("effect claims");
    assert!(matches!(first, EffectClaim::Acquired(_)));
    let after_crash = store
        .claim_effect(
            "session_effect",
            "turn_effect",
            "write:workspace/config",
            ExecutionPhase::Tool,
        )
        .expect("existing effect reads");
    assert!(
        matches!(after_crash, EffectClaim::Uncertain(_)),
        "an ambiguous claimed effect must not be executed again"
    );

    store
        .commit_effect(
            "session_effect",
            "turn_effect",
            "write:workspace/config",
            Some(json!({"path": "config"})),
        )
        .expect("effect commits");
    let committed = store
        .claim_effect(
            "session_effect",
            "turn_effect",
            "write:workspace/config",
            ExecutionPhase::Tool,
        )
        .expect("committed effect reads");
    assert!(matches!(committed, EffectClaim::AlreadyCommitted(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn lifecycle_ledger_serializes_cross_process_writers() {
    const CHILD_ROOT_ENV: &str = "OPENAGENT_DURABLE_LEDGER_TEST_ROOT";
    const CHILD_WORKER_ENV: &str = "OPENAGENT_DURABLE_LEDGER_TEST_WORKER";
    if let (Some(root), Some(worker)) = (
        std::env::var_os(CHILD_ROOT_ENV),
        std::env::var_os(CHILD_WORKER_ENV),
    ) {
        let store = DurableExecutionStore::new(PathBuf::from(root));
        let worker = worker.to_string_lossy();
        for index in 0..20 {
            store
                .create(NewExecution::turn(
                    "session_concurrent",
                    format!("turn_{worker}_{index}"),
                    format!("request-{worker}-{index}"),
                ))
                .expect("child execution creates");
        }
        return;
    }

    let root = unique_temp_dir("openagent-durable-cross-process");
    let current_executable = std::env::current_exe().expect("test executable resolves");
    let mut children = ["a", "b"].map(|worker| {
        Command::new(&current_executable)
            .args([
                "--exact",
                "lifecycle_ledger_serializes_cross_process_writers",
            ])
            .env(CHILD_ROOT_ENV, &root)
            .env(CHILD_WORKER_ENV, worker)
            .spawn()
            .expect("child writer starts")
    });
    for child in &mut children {
        assert!(child.wait().expect("child writer exits").success());
    }

    let events = DurableExecutionStore::new(&root)
        .read_events("session_concurrent")
        .expect("concurrent ledger parses");
    assert_eq!(events.len(), 40);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=40).collect::<Vec<_>>()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn sqlite_catalog_rebuilds_history_tree_leases_and_full_text_from_ledgers() {
    let root = unique_temp_dir("openagent-durable-catalog");
    let session_store = openagent_session::FileSessionStore::new(&root);
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace creates");
    let mut session = Session::new("session_catalog", &workspace);
    session.status = SessionStatus::Running;
    session
        .metadata
        .insert("title".to_string(), json!("Catalog recovery"));
    session_store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "turn_catalog".to_string(),
                trace_id: "trace_catalog".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 4,
                started_at_ms: Some(10),
            },
        )
        .expect("run starts");
    let message = ChatMessage {
        role: Role::User,
        content: "durable catalog searchable sentinel".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_catalog"))]),
    };
    session.add(message.clone());
    session_store
        .append_message(&session, &message, "turn_catalog", 0)
        .expect("message appends");
    session_store
        .save_state(&session, Some("turn_catalog"))
        .expect("state saves");

    let execution_store = DurableExecutionStore::new(&root);
    let mut turn = NewExecution::turn("session_catalog", "turn_catalog", "request-catalog-turn");
    turn.status = ExecutionStatus::Running;
    turn.phase = ExecutionPhase::Provider;
    execution_store.create(turn).expect("turn creates");
    execution_store
        .heartbeat("session_catalog", "turn_catalog", "runtime-a", 30_000)
        .expect("lease heartbeats");
    let task = NewExecution {
        execution_id: "task_catalog".to_string(),
        session_id: "session_catalog".to_string(),
        kind: ExecutionKind::Task,
        parent_execution_id: Some("turn_catalog".to_string()),
        status: ExecutionStatus::Waiting,
        phase: ExecutionPhase::Approval,
        attempt: 1,
        idempotency_key: "request-catalog-task".to_string(),
        lease: None,
        metadata: BTreeMap::new(),
    };
    execution_store.create(task).expect("task creates");

    let catalog = DurableSessionCatalog::open(&root).expect("catalog opens");
    let report = catalog.rebuild(&root).expect("catalog rebuilds");
    assert_eq!(report.session_count, 1);
    assert_eq!(report.execution_count, 2);
    assert_eq!(report.message_count, 1);
    assert!(report.lifecycle_event_count >= 3);
    assert_eq!(
        catalog
            .list_sessions(Some("Catalog recovery"), 10)
            .expect("sessions query")
            .len(),
        1
    );
    let executions = catalog
        .list_executions("session_catalog")
        .expect("execution tree queries");
    assert_eq!(executions.len(), 2);
    assert!(
        executions
            .iter()
            .any(|record| record.parent_execution_id.as_deref() == Some("turn_catalog"))
    );
    assert!(executions.iter().any(|record| {
        record
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner_id == "runtime-a")
    }));
    let hits = catalog
        .search_messages("searchable sentinel", 10)
        .expect("full text searches");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id, "msg_catalog");

    let catalog_path = catalog.path().to_path_buf();
    drop(catalog);
    fs::remove_file(&catalog_path).expect("catalog is disposable");
    let rebuilt = DurableSessionCatalog::open(&root).expect("catalog reopens");
    let second_report = rebuilt.rebuild(&root).expect("catalog rebuilds again");
    assert_eq!(second_report.execution_count, 2);
    assert_eq!(
        rebuilt
            .search_messages("durable catalog", 10)
            .expect("rebuilt full text searches")
            .len(),
        1
    );
    let _ = fs::remove_dir_all(root);
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after UNIX epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
    fs::create_dir_all(&path).expect("temp dir creates");
    path
}

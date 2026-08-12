use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use openagent_protocol::{ChatMessage, Role, Usage};
use openagent_protocol::{MessagePartKind, MessageStatus, message_parts_to_chat_messages};
use openagent_session::{
    AgentTraceRecorder, FileSessionStore, ObservationConfig, ObservationEvent,
    ObservationEventOptions, ObservationRecorder, ObservationTraceRecord, RunRecord,
    RuntimeLogRecord, RuntimeLogger, RuntimeLoggingConfig, RuntimeWarningConfig,
    RuntimeWarningRecord, Session, SessionEventOptions, SessionForkBoundary, SessionPartOptions,
    SessionStatus, StartRunOptions, TodoItem, TraceConfig, TraceEvent, TraceEventOptions,
    check_trace_run, format_runtime_warning_event, input_preview, load_trace_events,
    load_trace_summary, output_stats, render_trace_summary, sanitize_observation_value,
    sanitize_trace_value, step_usage_warnings,
};
use openagent_telemetry::{
    AgentIdentity, ModelIdentity, RunSurface, RuntimeBudgets, TaskContractV1, TraceContext,
    VersionIdentity,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn session_trace_observability_fixture_matches_legacy_oracle() {
    let fixture = fixture();

    assert_eq!(fixture["session"]["todo"], value(fixture_todo()));
    assert_eq!(fixture["session"]["message"], value(fixture_message()));
    assert_eq!(fixture["session"]["event"], fixture_session_event());
    assert_eq!(fixture["session"]["part"], fixture_session_part());
    assert_eq!(fixture["session"]["state"], fixture_session_state());
    assert_eq!(fixture["session"]["summary"], fixture_session_summary());

    assert_eq!(fixture["trace"]["config"], value(fixture_trace_config()));
    assert_eq!(fixture["trace"]["run"], value(fixture_run_record()));
    assert_eq!(fixture["trace"]["event"], value(fixture_trace_event()));
    assert_eq!(
        fixture["trace"]["rendered_summary"],
        json!(render_trace_summary(&fixture["trace"]["summary"]))
    );

    assert_eq!(
        fixture["observability"]["config"],
        value(fixture_observation_config())
    );
    assert_eq!(
        fixture["observability"]["trace"],
        value(fixture_observation_trace())
    );
    assert_eq!(
        fixture["observability"]["event"],
        value(fixture_observation_event())
    );
    assert_eq!(
        fixture["observability"]["input_preview"],
        json!(input_preview(
            json!({"api_key": "secret", "path": "README.md"}),
            80
        ))
    );
    assert_eq!(
        fixture["observability"]["output_stats"],
        value(output_stats("one\ntwo\n"))
    );

    assert_eq!(
        fixture["runtime_logging"]["config"],
        value(fixture_logging_config())
    );
    assert_eq!(
        fixture["runtime_logging"]["record"],
        value(fixture_log_record())
    );

    let warning = fixture_warning_record();
    assert_eq!(
        fixture["runtime_warnings"]["config"],
        value(fixture_warning_config())
    );
    assert_eq!(fixture["runtime_warnings"]["record"], value(&warning));
    assert_eq!(fixture["runtime_warnings"]["event"], warning.to_event());
    assert_eq!(
        fixture["runtime_warnings"]["formatted"],
        json!(format_runtime_warning_event(&warning.to_event()).expect("warning formats"))
    );
}

#[test]
fn file_session_store_persists_validated_task_contract() {
    let root = unique_temp_dir("openagent-session-contract");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_contract", root.join("workspace"));
    let trace = TraceContext::new_root(true);
    let contract = TaskContractV1::new(
        "task_contract",
        session.id.clone(),
        "run_contract",
        RunSurface::Http,
        AgentIdentity {
            name: "server".to_string(),
            version: "agent-v1".to_string(),
        },
        ModelIdentity {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            family: "gpt".to_string(),
        },
        VersionIdentity::current_harness(
            "agent-v1",
            "prompt-v1",
            "skills-v1",
            "tools-v1",
            &json!({"temperature": 0}),
        ),
        RuntimeBudgets {
            max_steps: Some(4),
            ..RuntimeBudgets::default()
        },
        trace.clone(),
        1_781_840_000_000,
    );
    let metadata = store
        .start_run_with_contract(
            &mut session,
            StartRunOptions {
                run_id: contract.run_id.clone(),
                trace_id: trace.trace_id,
                agent_name: contract.agent.name.clone(),
                model_id: Some(contract.model.model.clone()),
                provider_id: Some(contract.model.provider.clone()),
                permission: "FULL".to_string(),
                max_steps: 4,
                started_at_ms: Some(contract.created_at_ms),
            },
            &contract,
        )
        .expect("contract run starts");
    let run: Value = serde_json::from_str(
        &fs::read_to_string(PathBuf::from(metadata.run_dir).join("run.json"))
            .expect("run record reads"),
    )
    .expect("run record parses");
    assert_eq!(
        run["task_contract"]["schema_version"],
        json!("openharness.task.v1")
    );
    assert_eq!(run["trace_id"], json!(contract.trace.trace_id));
    assert_eq!(
        run["task_contract"]["versions"]["config_fingerprint"],
        json!(contract.versions.config_fingerprint)
    );
    let run_event: Value = serde_json::from_str(
        fs::read_to_string(metadata.ledger_path)
            .expect("run event reads")
            .lines()
            .next()
            .expect("run event exists"),
    )
    .expect("run event parses");
    assert_eq!(run_event["trace_id"], json!(contract.trace.trace_id));
    assert_eq!(run_event["span_id"], json!(contract.trace.span_id));
}

#[test]
fn file_session_store_writes_summary_and_restores_state() {
    let root = unique_temp_dir("openagent-session-store");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_test", root.join("workspace"));
    session.status = SessionStatus::Running;
    session.set_todos(vec![TodoItem::new(
        "port session store",
        "in_progress",
        "high",
        "todo-1",
    )]);

    let metadata = store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_test".to_string(),
                trace_id: "trace_test".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1_781_840_000_000),
            },
        )
        .expect("run starts");
    let message = ChatMessage {
        role: Role::User,
        content: "hello".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_1"))]),
    };
    session.add(message.clone());
    store
        .append_message(&session, &message, "run_test", 0)
        .expect("message appends");
    store
        .record_event(
            "session_test",
            "run_test",
            "model.usage",
            SessionEventOptions {
                kind: "model".to_string(),
                attributes: BTreeMap::from([
                    ("input_tokens".to_string(), json!(5)),
                    ("output_tokens".to_string(), json!(7)),
                    ("cost".to_string(), json!(0.02)),
                ]),
                ..SessionEventOptions::default()
            },
        )
        .expect("usage event records");
    store
        .append_part(
            "session_test",
            "run_test",
            "usage",
            SessionPartOptions {
                attributes: BTreeMap::from([
                    ("input_tokens".to_string(), json!(5)),
                    ("output_tokens".to_string(), json!(7)),
                ]),
                step_index: Some(1),
                ..SessionPartOptions::default()
            },
        )
        .expect("part appends");
    session.status = SessionStatus::Idle;
    store
        .finish_run(&session, "run_test", "completed", 1, Some("stop"), None)
        .expect("run finishes");

    let summary = read_json(root.join("sessions/session_test/runs/run_test/summary.json"));
    assert_eq!(summary["status"], "completed");
    assert_eq!(summary["message_count"], 1);
    assert_eq!(summary["part_type_counts"]["usage"], 1);
    assert_eq!(summary["total_input_tokens"], 5);
    assert_eq!(summary["total_output_tokens"], 7);

    let restored = store
        .load_session("session_test")
        .expect("session restores");
    assert_eq!(restored.id, "session_test");
    assert_eq!(restored.messages[0].content, "hello");
    assert_eq!(restored.todos[0].id, "todo-1");
    assert_eq!(
        restored.metadata["session_store"]["ledger_path"],
        json!(metadata.ledger_path)
    );
    let parts = store
        .load_parts("session_test", "run_test")
        .expect("parts load");
    assert_eq!(parts[0].part_type, "usage");

    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_interrupted".to_string(),
                trace_id: "trace_interrupted".to_string(),
                agent_name: "default".to_string(),
                model_id: Some("gpt-test".to_string()),
                provider_id: Some("test".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1_000),
            },
        )
        .expect("interrupted run starts");
    session.status = SessionStatus::Stop;
    store
        .finish_run(
            &session,
            "run_interrupted",
            "interrupted",
            1,
            Some("interrupted"),
            Some("interrupt requested"),
        )
        .expect("interrupted run finishes");
    let interrupted_summary =
        read_json(root.join("sessions/session_test/runs/run_interrupted/summary.json"));
    assert_eq!(interrupted_summary["status"], "interrupted");

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_writes_message_v2_parts_and_attaches_tool_results() {
    let root = unique_temp_dir("openagent-message-v2-store");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_msg_v2", root.join("workspace"));
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_msg_v2".to_string(),
                trace_id: "trace_msg_v2".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");

    let assistant = ChatMessage {
        role: Role::Assistant,
        content: "I'll read it.".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([
            ("message_id".to_string(), json!("msg_assistant")),
            ("step".to_string(), json!(1)),
            (
                "tool_calls".to_string(),
                json!([{
                    "id": "call_read",
                    "call_id": "call_read",
                    "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"Cargo.toml\"}"},
                    "name": "read",
                    "input": {"path": "Cargo.toml"}
                }]),
            ),
        ]),
    };
    session.add(assistant.clone());
    store
        .append_message(&session, &assistant, "run_msg_v2", 0)
        .expect("assistant appends");

    let tool = ChatMessage {
        role: Role::Tool,
        content: "[workspace]".to_string(),
        name: Some("read".to_string()),
        tool_call_id: Some("call_read".to_string()),
        metadata: BTreeMap::from([
            ("assistant_message_id".to_string(), json!("msg_assistant")),
            ("step".to_string(), json!(1)),
            (
                "tool_result".to_string(),
                json!({
                    "call_id": "call_read",
                    "output": "[workspace]",
                    "error": null,
                    "metadata": {}
                }),
            ),
        ]),
    };
    session.add(tool.clone());
    store
        .append_message(&session, &tool, "run_msg_v2", 1)
        .expect("tool appends");

    let messages = store
        .list_messages_with_parts("session_msg_v2", None, None)
        .expect("messages list");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].info.id, "msg_assistant");
    assert!(messages[0].parts.iter().any(|part| {
        part.kind == MessagePartKind::Tool && part.status == MessageStatus::Pending
    }));
    assert!(messages[0].parts.iter().any(|part| {
        part.kind == MessagePartKind::Tool && part.status == MessageStatus::Completed
    }));

    let projected = message_parts_to_chat_messages(&messages);
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].role, Role::Assistant);
    assert_eq!(projected[1].role, Role::Tool);
    assert_eq!(projected[1].tool_call_id.as_deref(), Some("call_read"));
    assert_eq!(projected[1].content, "[workspace]");

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_loads_transcript_v2_when_state_is_stale() {
    let root = unique_temp_dir("openagent-stale-state-recovery");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_stale", root.join("workspace"));
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_stale".to_string(),
                trace_id: "trace_stale".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");

    let user = ChatMessage {
        role: Role::User,
        content: "persist me before provider".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_user_stale"))]),
    };
    session.add(user.clone());
    store
        .append_message(&session, &user, "run_stale", 0)
        .expect("message appends after stale state save");

    let restored = store
        .load_session("session_stale")
        .expect("session restores from transcript projection");
    assert_eq!(restored.messages.len(), 1);
    assert_eq!(restored.messages[0].content, "persist me before provider");
    assert_eq!(
        restored.messages[0].metadata["message_id"],
        json!("msg_user_stale")
    );

    store
        .save_state(&session, Some("run_stale"))
        .expect("state saves projection cache");
    let state = read_json(root.join("sessions/session_stale/state.latest.json"));
    assert_eq!(state["message_source"], json!("transcript_v2"));
    assert_eq!(
        state["messages_v2"][0]["info"]["id"],
        json!("msg_user_stale")
    );

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_marks_unfinished_tool_calls_interrupted_on_replay() {
    let root = unique_temp_dir("openagent-interrupted-tool-recovery");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_interrupted", root.join("workspace"));
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_interrupted".to_string(),
                trace_id: "trace_interrupted".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");

    let assistant = ChatMessage {
        role: Role::Assistant,
        content: String::new(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([
            ("message_id".to_string(), json!("msg_assistant_pending")),
            ("step".to_string(), json!(1)),
            (
                "tool_calls".to_string(),
                json!([{
                    "id": "call_write",
                    "call_id": "call_write",
                    "type": "function",
                    "function": {"name": "write", "arguments": "{\"path\":\"note.txt\",\"content\":\"hi\"}"},
                    "name": "write",
                    "input": {"path": "note.txt", "content": "hi"}
                }]),
            ),
        ]),
    };
    session.add(assistant.clone());
    store
        .append_message(&session, &assistant, "run_interrupted", 0)
        .expect("assistant appends");

    let messages = store
        .list_messages_with_parts("session_interrupted", None, None)
        .expect("messages replay");
    let tool_part = messages[0]
        .parts
        .iter()
        .find(|part| part.kind == MessagePartKind::Tool)
        .expect("tool part exists");
    assert_eq!(tool_part.status, MessageStatus::Interrupted);
    assert_eq!(tool_part.attributes["interrupted"], json!(true));

    let projected = message_parts_to_chat_messages(&messages);
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[1].role, Role::Tool);
    assert_eq!(projected[1].tool_call_id.as_deref(), Some("call_write"));
    assert!(
        projected[1]
            .content
            .contains("Tool call interrupted before completion")
    );

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_keeps_tool_call_pending_while_waiting_for_question() {
    let root = unique_temp_dir("openagent-pending-question-tool-recovery");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_pending_question", root.join("workspace"));
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_pending_question".to_string(),
                trace_id: "trace_pending_question".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");

    let assistant = ChatMessage {
        role: Role::Assistant,
        content: String::new(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([
            ("message_id".to_string(), json!("msg_assistant_question")),
            ("step".to_string(), json!(1)),
            (
                "tool_calls".to_string(),
                json!([{
                    "id": "call_question",
                    "call_id": "call_question",
                    "type": "function",
                    "function": {"name": "question", "arguments": "{\"questions\":[]}"},
                    "name": "question",
                    "input": {"questions": []}
                }]),
            ),
        ]),
    };
    session.add(assistant.clone());
    store
        .append_message(&session, &assistant, "run_pending_question", 0)
        .expect("assistant appends");
    store
        .append_part(
            "session_pending_question",
            "run_pending_question",
            "question",
            SessionPartOptions {
                message_id: Some("msg_assistant_question".to_string()),
                content: Some(json!({
                    "call_id": "call_question",
                    "name": "question",
                    "status": "pending",
                })),
                attributes: BTreeMap::from([
                    ("call_id".to_string(), json!("call_question")),
                    ("name".to_string(), json!("question")),
                ]),
                step_index: Some(1),
                status: "pending".to_string(),
                ..SessionPartOptions::default()
            },
        )
        .expect("question part appends");

    let messages = store
        .list_messages_with_parts("session_pending_question", None, None)
        .expect("messages replay");
    let tool_part = messages[0]
        .parts
        .iter()
        .find(|part| part.kind == MessagePartKind::Tool)
        .expect("tool part exists");
    assert_eq!(tool_part.status, MessageStatus::Pending);
    assert!(!tool_part.attributes.contains_key("interrupted"));

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_checkpoints_diff_and_restore_workspace() {
    let root = unique_temp_dir("openagent-checkpoint-restore");
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace exists");
    fs::write(workspace.join("keep.txt"), "before").expect("file writes");
    fs::write(workspace.join("delete.txt"), "delete me").expect("file writes");

    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_checkpoint", &workspace);
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_checkpoint".to_string(),
                trace_id: "trace_checkpoint".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");
    let before = store
        .create_checkpoint(
            "session_checkpoint",
            "run_checkpoint",
            &workspace,
            "step_start",
            Some("msg_checkpoint"),
            None,
            Some(1),
        )
        .expect("checkpoint creates");

    fs::write(workspace.join("keep.txt"), "after").expect("file modifies");
    fs::remove_file(workspace.join("delete.txt")).expect("file deletes");
    fs::write(workspace.join("added.txt"), "new").expect("file adds");
    fs::create_dir_all(workspace.join(".openagent")).expect("state dir exists");
    fs::write(workspace.join(".openagent/ignored.txt"), "ignored").expect("ignored writes");

    let after = store
        .create_checkpoint(
            "session_checkpoint",
            "run_checkpoint",
            &workspace,
            "step_end",
            Some("msg_checkpoint"),
            None,
            Some(1),
        )
        .expect("checkpoint creates");
    let diff = store
        .diff_checkpoints(
            "session_checkpoint",
            &before.checkpoint_id,
            &after.checkpoint_id,
        )
        .expect("checkpoint diff loads");
    assert_eq!(diff.added, 1);
    assert_eq!(diff.modified, 1);
    assert_eq!(diff.deleted, 1);
    assert!(
        diff.entries
            .iter()
            .all(|entry| !entry.path.contains(".openagent"))
    );

    store
        .restore_checkpoint(
            "session_checkpoint",
            "run_checkpoint",
            &workspace,
            &before.checkpoint_id,
        )
        .expect("checkpoint restores");
    assert_eq!(
        fs::read_to_string(workspace.join("keep.txt")).expect("file reads"),
        "before"
    );
    assert!(workspace.join("delete.txt").exists());
    assert!(!workspace.join("added.txt").exists());

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_forks_and_truncates_message_v2_transcript() {
    let root = unique_temp_dir("openagent-fork-truncate");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_source", root.join("workspace"));
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_source".to_string(),
                trace_id: "trace_source".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");
    for (index, (message_id, content)) in [
        ("msg_source_1", "first"),
        ("msg_source_2", "second"),
        ("msg_source_3", "third"),
    ]
    .into_iter()
    .enumerate()
    {
        let message = ChatMessage {
            role: Role::User,
            content: content.to_string(),
            name: None,
            tool_call_id: None,
            metadata: BTreeMap::from([("message_id".to_string(), json!(message_id))]),
        };
        session.add(message.clone());
        store
            .append_message(&session, &message, "run_source", index as u64)
            .expect("message appends");
    }

    let forked = store
        .fork_session(
            "session_source",
            "session_forked",
            root.join("forked_workspace"),
            Some(SessionForkBoundary {
                message_id: Some("msg_source_2".to_string()),
                part_id: None,
            }),
        )
        .expect("session forks");
    assert_eq!(forked.messages.len(), 2);
    assert_eq!(forked.messages[1].content, "second");
    let forked_messages = store
        .list_messages_with_parts("session_forked", None, None)
        .expect("forked messages load");
    assert_eq!(forked_messages.len(), 2);
    assert!(
        forked_messages[0]
            .info
            .id
            .starts_with("msg_session_forked_")
    );
    assert_eq!(
        forked_messages[0].info.metadata["forked_from_message_id"],
        json!("msg_source_1")
    );

    store
        .truncate_messages_after(
            "session_source",
            "run_source",
            SessionForkBoundary {
                message_id: Some("msg_source_1".to_string()),
                part_id: None,
            },
        )
        .expect("source truncates");
    let truncated = store
        .list_messages_with_parts("session_source", None, None)
        .expect("truncated messages load");
    assert_eq!(truncated.len(), 1);
    assert_eq!(truncated[0].info.id, "msg_source_1");

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_compaction_boundary_skips_compacted_context() {
    let root = unique_temp_dir("openagent-compaction-boundary");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_compact", root.join("workspace"));
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_compact".to_string(),
                trace_id: "trace_compact".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");
    for (index, (message_id, content)) in [
        ("msg_compact_1", "old context"),
        ("msg_compact_2", "also old"),
    ]
    .into_iter()
    .enumerate()
    {
        let message = ChatMessage {
            role: Role::User,
            content: content.to_string(),
            name: None,
            tool_call_id: None,
            metadata: BTreeMap::from([("message_id".to_string(), json!(message_id))]),
        };
        session.add(message.clone());
        store
            .append_message(&session, &message, "run_compact", index as u64)
            .expect("message appends");
    }
    let boundary_id = store
        .append_compaction_boundary(
            &mut session,
            "run_compact",
            "Summary of compacted context.",
            "msg_compact_2",
        )
        .expect("compaction boundary appends");
    let current = ChatMessage {
        role: Role::User,
        content: "fresh context".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_compact_3"))]),
    };
    session.add(current.clone());
    store
        .append_message(&session, &current, "run_compact", 3)
        .expect("fresh message appends");

    let messages = store
        .list_messages_with_parts("session_compact", None, None)
        .expect("messages load");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].info.id, boundary_id);
    assert_eq!(messages[1].info.id, "msg_compact_3");
    let projected = message_parts_to_chat_messages(&messages);
    assert_eq!(projected[0].content, "Summary of compacted context.");
    assert_eq!(projected[1].content, "fresh context");

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_compaction_boundary_preserves_loaded_skill_output() {
    let root = unique_temp_dir("openagent-compaction-boundary-skill");
    let store = FileSessionStore::new(root.join("sessions"));
    let mut session = Session::new("session_compact_skill", root.join("workspace"));
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "run_compact_skill".to_string(),
                trace_id: "trace_compact_skill".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");

    let old = ChatMessage {
        role: Role::User,
        content: "old context".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_skill_old"))]),
    };
    session.add(old.clone());
    store
        .append_message(&session, &old, "run_compact_skill", 0)
        .expect("old message appends");

    let mut skill_tool = ChatMessage {
        role: Role::Tool,
        content: "<skill_content name=\"code-review\">Use code review guidance.</skill_content>"
            .to_string(),
        name: Some("skill".to_string()),
        tool_call_id: Some("call_skill".to_string()),
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_skill_loaded"))]),
    };
    skill_tool.metadata.insert(
        "tool_result".to_string(),
        json!({
            "call_id": "call_skill",
            "output": skill_tool.content.clone(),
            "error": null,
            "metadata": {
                "skill_name": "code-review",
                "skill_dir": "/tmp/skills/code-review",
                "skill_files": ["/tmp/skills/code-review/references/checklist.md"]
            }
        }),
    );
    session.add(skill_tool.clone());
    store
        .append_message(&session, &skill_tool, "run_compact_skill", 1)
        .expect("skill tool message appends");

    let compacted_until = ChatMessage {
        role: Role::User,
        content: "also old".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_skill_cutoff"))]),
    };
    session.add(compacted_until.clone());
    store
        .append_message(&session, &compacted_until, "run_compact_skill", 2)
        .expect("cutoff message appends");

    let boundary_id = store
        .append_compaction_boundary(
            &mut session,
            "run_compact_skill",
            "Summary of compacted context.",
            "msg_skill_cutoff",
        )
        .expect("compaction boundary appends");
    let fresh = ChatMessage {
        role: Role::User,
        content: "fresh context".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_skill_fresh"))]),
    };
    session.add(fresh.clone());
    store
        .append_message(&session, &fresh, "run_compact_skill", 4)
        .expect("fresh message appends");

    let messages = store
        .list_messages_with_parts("session_compact_skill", None, None)
        .expect("messages load");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].info.id, "msg_skill_loaded");
    assert_eq!(messages[1].info.id, boundary_id);
    assert_eq!(messages[2].info.id, "msg_skill_fresh");
    let skill_part = messages[0]
        .parts
        .iter()
        .find(|part| part.kind == MessagePartKind::Tool)
        .expect("skill tool part preserved");
    assert_eq!(skill_part.content["metadata"]["skill_name"], "code-review");
    assert!(
        skill_part.content["output"]
            .as_str()
            .is_some_and(|output| output.contains("Use code review guidance."))
    );
    let projected = message_parts_to_chat_messages(&messages);
    assert!(projected.iter().any(|message| message.role == Role::Tool
        && message.content.contains("Use code review guidance.")));

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn file_session_store_projects_legacy_v1_transcripts_to_message_v2() {
    let root = unique_temp_dir("openagent-message-v2-legacy");
    let session_dir = root.join("sessions/session_legacy");
    fs::create_dir_all(&session_dir).expect("session dir exists");
    fs::write(
        session_dir.join("transcript.jsonl"),
        serde_json::to_string(&json!({
            "schema_version": "openagent.message.v1",
            "message_id": "msg_legacy",
            "session_id": "session_legacy",
            "run_id": "run_legacy",
            "index": 0,
            "role": "user",
            "content": "hello from v1",
            "name": null,
            "tool_call_id": null,
            "metadata": {},
            "timestamp_ms": 1,
        }))
        .expect("json serializes")
            + "\n",
    )
    .expect("legacy transcript writes");

    let store = FileSessionStore::new(root.join("sessions"));
    let messages = store
        .list_messages_with_parts("session_legacy", None, None)
        .expect("legacy projects");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].info.id, "msg_legacy");
    assert_eq!(messages[0].parts[0].kind, MessagePartKind::Text);
    assert_eq!(messages[0].parts[0].content, json!("hello from v1"));

    fs::remove_dir_all(root).expect("temporary session store is removed");
}

#[test]
fn trace_recorder_writes_jsonl_summary_and_checkable_run() {
    let root = unique_temp_dir("openagent-trace");
    let run = RunRecord {
        run_id: "run_trace".to_string(),
        trace_id: "trace_trace".to_string(),
        session_id: "session_trace".to_string(),
        agent_name: "agent".to_string(),
        model_id: Some("model".to_string()),
        provider_id: Some("provider".to_string()),
        workspace: Some(root.to_string_lossy().to_string()),
        started_at_ms: 1_781_840_000_000,
    };
    let mut metadata = BTreeMap::new();
    {
        let mut recorder = AgentTraceRecorder::new(
            run,
            Some(TraceConfig {
                root_dir: "runs".to_string(),
                ..TraceConfig::default()
            }),
            &root,
            Some(&mut metadata),
        )
        .expect("trace recorder starts");
        recorder
            .record_event(
                "run.started",
                TraceEventOptions {
                    kind: Some("run".to_string()),
                    ..TraceEventOptions::default()
                },
            )
            .expect("run start records");
        recorder
            .record_event(
                "step.started",
                TraceEventOptions {
                    kind: Some("step".to_string()),
                    ..TraceEventOptions::default()
                },
            )
            .expect("step start records");
        recorder
            .record_event(
                "model.call.started",
                TraceEventOptions {
                    kind: Some("model".to_string()),
                    span_id: Some("model_1".to_string()),
                    ..TraceEventOptions::default()
                },
            )
            .expect("model start records");
        recorder
            .record_event(
                "model.call.finished",
                TraceEventOptions {
                    kind: Some("model".to_string()),
                    span_id: Some("model_1".to_string()),
                    duration_ms: Some(33),
                    attributes: BTreeMap::from([
                        ("api_key".to_string(), json!("secret")),
                        ("input_tokens".to_string(), json!(3)),
                        ("output_tokens".to_string(), json!(4)),
                        ("cost".to_string(), json!(0.01)),
                    ]),
                    ..TraceEventOptions::default()
                },
            )
            .expect("model finish records");
        recorder
            .record_event(
                "step.finished",
                TraceEventOptions {
                    kind: Some("step".to_string()),
                    ..TraceEventOptions::default()
                },
            )
            .expect("step finish records");
        recorder
            .finish_run("completed", BTreeMap::new())
            .expect("run finish records");
    }

    let run_dir = root.join("runs/run_trace");
    let check = check_trace_run(&run_dir).expect("trace check runs");
    assert_eq!(check["ok"], true);
    let events = load_trace_events(run_dir.join("trace.jsonl")).expect("trace events load");
    assert_eq!(events[3]["attributes"]["api_key"], "[redacted]");
    let summary = load_trace_summary(run_dir.join("summary.json")).expect("summary loads");
    assert_eq!(summary["status"], "completed");
    assert_eq!(summary["model_call_count"], 1);
    assert_eq!(summary["total_input_tokens"], 3);
    assert_eq!(metadata["agent_trace"]["run_id"], "run_trace");

    fs::remove_dir_all(root).expect("temporary trace root is removed");
}

#[test]
fn observability_logging_and_warnings_write_sanitized_records() {
    let root = unique_temp_dir("openagent-observability");
    let mut metadata = BTreeMap::new();
    {
        let trace = fixture_observation_trace();
        let mut recorder = ObservationRecorder::new(
            trace,
            Some(ObservationConfig {
                jsonl: true,
                jsonl_dir: "observability".to_string(),
                max_events: 2,
                ..ObservationConfig::default()
            }),
            &root,
            &mut metadata,
        );
        recorder
            .event(
                "tool.call.finished",
                "tool",
                BTreeMap::from([
                    ("token".to_string(), json!("secret")),
                    ("output_lines".to_string(), json!(2)),
                ]),
                ObservationEventOptions {
                    event_id: Some("event_1".to_string()),
                    span_id: Some("span_tool".to_string()),
                    parent_span_id: Some("span_step".to_string()),
                    ..ObservationEventOptions::default()
                },
            )
            .expect("observation records");
    }
    assert_eq!(metadata["observability"]["event_count"], 1);
    assert_eq!(
        metadata["observability"]["events"][0]["attributes"]["token"],
        "[redacted]"
    );

    {
        let mut logger = RuntimeLogger::new(
            "session_fixture",
            &mut metadata,
            Some(RuntimeLoggingConfig {
                jsonl: true,
                jsonl_dir: "logs".to_string(),
                level: "WARNING".to_string(),
                ..RuntimeLoggingConfig::default()
            }),
            &root,
            Some("run_fixture".to_string()),
            Some("trace_fixture".to_string()),
        )
        .with_span_id(Some("span_fixture".to_string()));
        assert!(
            logger
                .log(
                    "INFO",
                    "ignored",
                    "runtime",
                    BTreeMap::new(),
                    Some(1_781_840_000_500),
                )
                .expect("info log filters")
                .is_none()
        );
        logger
            .log(
                "WARNING",
                "Tool output was truncated.",
                "tool",
                BTreeMap::from([("authorization".to_string(), json!("secret"))]),
                Some(1_781_840_000_501),
            )
            .expect("warning log records");
    }
    assert_eq!(metadata["runtime_logging"]["record_count"], 1);
    assert_eq!(metadata["runtime_logging"]["span_id"], "span_fixture");
    assert_eq!(
        metadata["runtime_logging"]["records"][0]["span_id"],
        "span_fixture"
    );
    assert_eq!(
        metadata["runtime_logging"]["records"][0]["attributes"]["authorization"],
        "[redacted]"
    );

    let warnings = step_usage_warnings(
        &RuntimeWarningConfig {
            enabled: true,
            max_step_total_tokens: Some(9),
            ..RuntimeWarningConfig::default()
        },
        &Usage {
            input_tokens: 5,
            output_tokens: 7,
            cost: 0.02,
        },
        1,
    );
    assert_eq!(warnings.len(), 1);
    let event = warnings[0].to_event();
    assert_eq!(event["display"]["title"], "Step token budget exceeded");
    assert!(
        format_runtime_warning_event(&event)
            .expect("warning formats")
            .contains("total_tokens=12")
    );

    fs::remove_dir_all(root).expect("temporary observability root is removed");
}

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/golden/rust_rewrite/session_trace_observability.json"
    ))
    .expect("fixture JSON parses")
}

fn fixture_todo() -> TodoItem {
    TodoItem::new("port session store", "in_progress", "high", "todo-fixture")
}

fn fixture_message() -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: "Remember this fixture.".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_fixture"))]),
    }
}

fn fixture_session_event() -> Value {
    json!({
        "schema_version": "openagent.session_event.v1",
        "seq": 1,
        "event": "model.usage",
        "timestamp_ms": 1781840000100_u64,
        "session_id": "session_fixture",
        "run_id": "run_fixture",
        "kind": "model",
        "status": "ok",
        "duration_ms": 12,
        "attributes": {
            "input_tokens": 11,
            "output_tokens": 7,
            "cost": 0.001,
            "authorization": "secret",
        },
    })
}

fn fixture_session_part() -> Value {
    json!({
        "schema_version": "openagent.session_part.v1",
        "part_id": "part_fixture",
        "seq": 1,
        "type": "usage",
        "timestamp_ms": 1781840000110_u64,
        "session_id": "session_fixture",
        "run_id": "run_fixture",
        "step_index": 1,
        "status": "ok",
        "attributes": {"input_tokens": 11, "output_tokens": 7},
    })
}

fn fixture_session_state() -> Value {
    json!({
        "schema_version": "openagent.session_state.v1",
        "session_id": "session_fixture",
        "run_id": "run_fixture",
        "workspace": "/tmp/openagent-fixture",
        "status": "idle",
        "updated_at_ms": 1781840000120_u64,
        "messages": [{
            "message_id": "msg_fixture",
            "index": 0,
            "role": "user",
            "content": "Remember this fixture.",
            "name": Value::Null,
            "tool_call_id": Value::Null,
            "metadata": {"message_id": "msg_fixture"},
        }],
        "todos": [fixture_todo()],
        "metadata": {
            "session_store": {
                "enabled": true,
                "type": "file",
                "root_dir": "/tmp/openagent-fixture/.openagent/sessions",
                "session_id": "session_fixture",
                "run_id": "run_fixture",
            }
        },
    })
}

fn fixture_session_summary() -> Value {
    json!({
        "schema_version": "openagent.run_summary.v1",
        "session_id": "session_fixture",
        "run_id": "run_fixture",
        "event_count": 2,
        "part_count": 1,
        "part_type_counts": {"usage": 1},
        "message_count": 1,
        "step_count": 0,
        "tool_call_count": 0,
        "runtime_warning_count": 0,
        "patch_count": 0,
        "total_input_tokens": 11,
        "total_output_tokens": 7,
        "total_cost": 0.001,
        "status": "completed",
    })
}

fn fixture_trace_config() -> TraceConfig {
    TraceConfig {
        root_dir: "runs".to_string(),
        max_events: 12,
        exporters: BTreeMap::from([("langfuse".to_string(), json!({"enabled": false}))]),
        ..TraceConfig::default()
    }
}

fn fixture_run_record() -> RunRecord {
    RunRecord {
        run_id: "run_fixture".to_string(),
        trace_id: "trace_fixture".to_string(),
        session_id: "session_fixture".to_string(),
        agent_name: "fixture-agent".to_string(),
        model_id: Some("fixture-model".to_string()),
        provider_id: Some("fixture-provider".to_string()),
        workspace: Some("/tmp/openagent-fixture".to_string()),
        started_at_ms: 1_781_840_000_000,
    }
}

fn fixture_trace_event() -> TraceEvent {
    let attrs = sanitize_trace_value(json!({
        "api_key": "secret",
        "input_tokens": 11,
        "output_tokens": 7,
        "cost": 0.001,
        "prompt": "P".repeat(4100),
    }));
    TraceEvent {
        seq: 1,
        event: "model.call.finished".to_string(),
        event_id: Some("event_fixture".to_string()),
        timestamp_ms: 1_781_840_000_200,
        run_id: "run_fixture".to_string(),
        trace_id: "trace_fixture".to_string(),
        session_id: "session_fixture".to_string(),
        kind: "model".to_string(),
        status: "ok".to_string(),
        span_id: Some("span_model".to_string()),
        parent_span_id: Some("span_step".to_string()),
        duration_ms: Some(25),
        attributes: attrs
            .as_object()
            .expect("attrs object")
            .clone()
            .into_iter()
            .collect(),
    }
}

fn fixture_observation_config() -> ObservationConfig {
    ObservationConfig {
        jsonl: true,
        jsonl_dir: "observability".to_string(),
        max_events: 3,
        ..ObservationConfig::default()
    }
}

fn fixture_observation_trace() -> ObservationTraceRecord {
    ObservationTraceRecord {
        trace_id: "trace_fixture".to_string(),
        session_id: "session_fixture".to_string(),
        run_id: "run_fixture".to_string(),
        agent_name: "fixture-agent".to_string(),
        model_id: Some("fixture-model".to_string()),
        provider_id: Some("fixture-provider".to_string()),
        workspace: Some("/tmp/openagent-fixture".to_string()),
        started_at_ms: 1_781_840_000_000,
    }
}

fn fixture_observation_event() -> ObservationEvent {
    let attrs = sanitize_observation_value(json!({
        "token": "secret",
        "output_lines": 2,
        "result_summary": "ok",
    }));
    ObservationEvent {
        event_id: "event_observation".to_string(),
        trace_id: "trace_fixture".to_string(),
        run_id: "run_fixture".to_string(),
        session_id: "session_fixture".to_string(),
        span_id: Some("span_tool".to_string()),
        parent_span_id: Some("span_step".to_string()),
        name: "tool.call.finished".to_string(),
        kind: "tool".to_string(),
        timestamp_ms: 1_781_840_000_400,
        duration_ms: Some(9),
        status: "ok".to_string(),
        attributes: attrs
            .as_object()
            .expect("attrs object")
            .clone()
            .into_iter()
            .collect(),
    }
}

fn fixture_logging_config() -> RuntimeLoggingConfig {
    RuntimeLoggingConfig {
        jsonl: true,
        jsonl_dir: "logs".to_string(),
        level: "WARNING".to_string(),
        structured_logging: false,
        ..RuntimeLoggingConfig::default()
    }
}

fn fixture_log_record() -> RuntimeLogRecord {
    let attrs = sanitize_observation_value(json!({"authorization": "secret", "output_lines": 2}));
    RuntimeLogRecord {
        log_id: "log_fixture".to_string(),
        timestamp_ms: 1_781_840_000_500,
        level: "WARNING".to_string(),
        message: "Tool output was truncated.".to_string(),
        category: "tool".to_string(),
        session_id: "session_fixture".to_string(),
        run_id: Some("run_fixture".to_string()),
        trace_id: Some("trace_fixture".to_string()),
        span_id: Some("span_tool".to_string()),
        attributes: attrs
            .as_object()
            .expect("attrs object")
            .clone()
            .into_iter()
            .collect(),
    }
}

fn fixture_warning_config() -> RuntimeWarningConfig {
    RuntimeWarningConfig {
        enabled: true,
        max_step_total_tokens: Some(12),
        ..RuntimeWarningConfig::default()
    }
}

fn fixture_warning_record() -> RuntimeWarningRecord {
    RuntimeWarningRecord {
        code: "step_total_tokens_exceeded".to_string(),
        severity: "warning".to_string(),
        message: "Step total tokens exceeded budget: 18 > 12.".to_string(),
        metrics: BTreeMap::from([
            ("step_index".to_string(), json!(1)),
            ("input_tokens".to_string(), json!(11)),
            ("output_tokens".to_string(), json!(7)),
            ("total_tokens".to_string(), json!(18)),
            ("threshold".to_string(), json!(12)),
        ]),
    }
}

fn value(payload: impl Serialize) -> Value {
    serde_json::to_value(payload).expect("payload serializes")
}

fn read_json(path: impl Into<PathBuf>) -> Value {
    serde_json::from_str(&fs::read_to_string(path.into()).expect("JSON file reads"))
        .expect("JSON file parses")
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after UNIX epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
    fs::create_dir_all(&path).expect("temp dir is created");
    path
}

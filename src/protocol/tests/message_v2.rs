use std::collections::BTreeMap;

use openagent_protocol::{
    MessageInfo, MessagePart, MessagePartKind, MessageStatus, MessageWithParts, Role,
    materialize_model_messages, message_parts_to_chat_messages,
};
use serde_json::{Value, json};

#[test]
fn materializes_tool_parts_without_legacy_metadata() {
    let messages = vec![
        message(
            "msg_user",
            Role::User,
            vec![text_part("msg_user", 1, Role::User, "read Cargo.toml")],
        ),
        message(
            "msg_assistant",
            Role::Assistant,
            vec![
                text_part("msg_assistant", 1, Role::Assistant, "I'll inspect it."),
                tool_part(
                    "msg_assistant",
                    2,
                    MessageStatus::Pending,
                    json!({"call_id": "call_read", "name": "read", "input": {"path": "Cargo.toml"}}),
                ),
                tool_part(
                    "msg_assistant",
                    3,
                    MessageStatus::Completed,
                    json!({"call_id": "call_read", "name": "read", "output": "[workspace]", "error": null}),
                ),
            ],
        ),
    ];

    let legacy = message_parts_to_chat_messages(&messages);
    assert_eq!(legacy.len(), 3);
    assert_eq!(legacy[1].role, Role::Assistant);
    assert!(legacy[1].metadata["tool_calls"].is_array());
    assert_eq!(legacy[2].role, Role::Tool);
    assert_eq!(legacy[2].tool_call_id.as_deref(), Some("call_read"));
    assert_eq!(legacy[2].content, "[workspace]");

    let wire = materialize_model_messages(Some("system"), &messages);
    assert_eq!(wire[0]["role"], "system");
    assert_eq!(wire[2]["role"], "assistant");
    assert_eq!(wire[2]["tool_calls"][0]["function"]["name"], "read");
    assert_eq!(wire[3]["role"], "tool");
    assert_eq!(wire[3]["tool_call_id"], "call_read");
}

#[test]
fn pending_tool_parts_are_closed_for_provider_materialization() {
    let messages = vec![message(
        "msg_assistant",
        Role::Assistant,
        vec![tool_part(
            "msg_assistant",
            1,
            MessageStatus::Running,
            json!({"call_id": "call_pending", "name": "bash", "input": {"command": "sleep 1"}}),
        )],
    )];

    let legacy = message_parts_to_chat_messages(&messages);
    assert_eq!(legacy.len(), 2);
    assert_eq!(legacy[0].role, Role::Assistant);
    assert_eq!(legacy[1].role, Role::Tool);
    assert_eq!(legacy[1].tool_call_id.as_deref(), Some("call_pending"));
    assert!(legacy[1].content.contains("Tool failed:"));
}

fn message(id: &str, role: Role, parts: Vec<MessagePart>) -> MessageWithParts {
    MessageWithParts {
        info: MessageInfo {
            id: id.to_string(),
            session_id: "session_test".to_string(),
            parent_message_id: None,
            seq: Some(0),
            role,
            created_at_ms: 1,
            updated_at_ms: Some(1),
            completed_at_ms: Some(1),
            run_id: Some("run_test".to_string()),
            step_index: Some(1),
            status: MessageStatus::Completed,
            metadata: BTreeMap::new(),
        },
        parts,
    }
}

fn text_part(message_id: &str, seq: u64, role: Role, text: &str) -> MessagePart {
    MessagePart {
        id: format!("prt_{message_id}_{seq}"),
        message_id: message_id.to_string(),
        session_id: "session_test".to_string(),
        seq,
        kind: MessagePartKind::Text,
        status: MessageStatus::Completed,
        content: Value::String(text.to_string()),
        attributes: BTreeMap::from([("role".to_string(), json!(role))]),
        timestamp_ms: 1,
        run_id: Some("run_test".to_string()),
        step_index: Some(1),
    }
}

fn tool_part(message_id: &str, seq: u64, status: MessageStatus, content: Value) -> MessagePart {
    MessagePart {
        id: format!("prt_{message_id}_{seq}"),
        message_id: message_id.to_string(),
        session_id: "session_test".to_string(),
        seq,
        kind: MessagePartKind::Tool,
        status,
        content,
        attributes: BTreeMap::new(),
        timestamp_ms: 1,
        run_id: Some("run_test".to_string()),
        step_index: Some(1),
    }
}

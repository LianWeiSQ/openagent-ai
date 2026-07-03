use super::*;
use ratatui::backend::TestBackend;
use std::{
    error::Error,
    fs,
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[test]
fn exposes_command_boundary() {
    assert_eq!(crate_name(), "openagent-tui");
    assert_eq!(command_name(), "openagent-tui");
    assert_eq!(client_crate_name(), "openagent-app-server-client");
    assert_eq!(server_crate_name(), "openagent-app-server");
}

#[test]
fn approval_events_render_diff_preview_and_support_allow_always() {
    let mut state = TuiState::new();
    let applied = state.apply_app_event(&json!({
        "method": "turn/approval_requested",
        "params": {
            "approval": {
                "request_id": "approval_1",
                "turn_id": "turn_1",
                "session_id": "session_1",
                "tool_name": "write",
                "tool_input": {"file_path": "src/core.rs"},
                "preview": {
                    "kind": "file",
                    "path": "src/core.rs",
                    "status": "modified",
                    "diff": "--- a/src/core.rs\n+++ b/src/core.rs\n+hello"
                }
            }
        }
    }));

    assert_eq!(applied["applied"], json!(true));
    assert_eq!(state.status, "approval pending");
    let timeline = state
        .timeline
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(timeline.contains("approval required: write"));
    assert!(timeline.contains("diff:"));
    assert!(timeline.contains("+hello"));

    let response = state.respond_active_approval("allow", Some("always"), None);

    assert_eq!(response["applied"], json!(true));
    assert_eq!(response["payload"]["action"], json!("allow"));
    assert_eq!(response["payload"]["scope"], json!("always"));
    assert_eq!(response["payload"]["request_id"], json!("approval_1"));
    assert!(state.active_approval.is_none());
}

#[test]
fn patch_events_render_structured_diff_and_undo_redo_markers() {
    let mut state = TuiState::new();
    let applied = state.apply_app_event(&json!({
        "method": "patch/detected",
        "params": {
            "session_id": "session_1",
            "turn_id": "turn_1",
            "patch": {
                "id": "patch_1",
                "path": "src/core.rs",
                "status": "modified",
                "diff": "--- a/src/core.rs\n+++ b/src/core.rs\n@@ -1,1 +1,1 @@\n-old\n+new"
            }
        }
    }));

    assert_eq!(applied["applied"], json!(true));
    assert_eq!(state.status, "patch detected");
    let kinds = state
        .timeline
        .iter()
        .map(|line| line.kind.as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"patch"));
    assert!(kinds.contains(&"diff-meta"));
    assert!(kinds.contains(&"diff-hunk"));
    assert!(kinds.contains(&"diff-del"));
    assert!(kinds.contains(&"diff-add"));
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("actions: /undo /redo"))
    );

    let detail_lines = diff_detail_lines(&json!({
        "undo_count": 1,
        "redo_count": 1,
        "latest": {
            "id": "patch_1",
            "path": "src/core.rs",
            "status": "modified",
            "diff": "+new"
        }
    }));
    assert_eq!(detail_lines[0].kind, "patch");
    assert!(detail_lines[0].text.contains("/undo"));
    assert!(detail_lines[0].text.contains("/redo"));
    assert!(detail_lines.iter().any(|line| line.kind == "diff-add"));
}

#[test]
fn approval_can_be_denied_with_note_from_command() {
    let mut state = TuiState::new();
    state.apply_app_event(&json!({
        "method": "turn/approval_requested",
        "params": {
            "approval": {
                "request_id": "approval_2",
                "turn_id": "turn_1",
                "tool_name": "bash",
                "tool_input": {"command": "rm -rf target"}
            }
        }
    }));
    state.input_buffer = "/deny risky command".to_string();

    assert!(!state.submit());

    let response = state.approval_responses.last().expect("approval response");
    assert_eq!(response["action"], json!("deny"));
    assert_eq!(response["note"], json!("risky command"));
    assert_eq!(state.status, "approval resolved");
}

#[test]
fn question_events_support_answer_and_dismiss() {
    let mut state = TuiState::new();
    let question_event = json!({
        "method": "item/question/requested",
        "params": {
            "event": {
                "type": "question-request",
                "request_id": "question_1",
                "turn_id": "turn_1",
                "session_id": "session_1",
                "tool_call_id": "call_question",
                "questions": [{
                    "header": "Plan",
                    "question": "Which option?",
                    "options": [
                        {"label": "Fast path", "description": "Move quickly"},
                        {"label": "Safe path", "description": "Be conservative"}
                    ]
                }]
            }
        }
    });

    assert_eq!(
        state.apply_app_event(&question_event)["applied"],
        json!(true)
    );
    assert_eq!(state.status, "question pending");
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("Fast path"))
    );

    state.input_buffer = "/answer Safe path".to_string();
    assert!(!state.submit());

    let response = state.question_responses.last().expect("question response");
    assert_eq!(response["answers"], json!([["Safe path"]]));
    assert_eq!(response["request_id"], json!("question_1"));
    assert!(state.active_question.is_none());

    state.apply_app_event(&question_event);
    let dismissed = state.dismiss_active_question(Some("not needed"));
    assert_eq!(dismissed["payload"]["dismissed"], json!(true));
    assert_eq!(dismissed["payload"]["note"], json!("not needed"));
}

#[test]
fn control_requests_can_respond_to_active_interactions() {
    let mut state = TuiState::new();
    state.apply_app_event(&json!({
        "method": "turn/approval_requested",
        "params": {"approval": {"request_id": "approval_3", "tool_name": "write"}}
    }));

    let approval = state.apply_control_request(&json!({
        "path": "/tui/respond-approval",
        "body": {"action": "allow_always"}
    }));

    assert_eq!(approval["applied"], json!(true));
    assert_eq!(approval["payload"]["scope"], json!("always"));

    state.apply_app_event(&json!({
        "method": "item/question/requested",
        "params": {"event": {"request_id": "question_2", "questions": [{"question": "Mode?"}]}}
    }));
    let answer = state.apply_control_request(&json!({
        "path": "/tui/reply-question",
        "body": {"answer": "Fast"}
    }));

    assert_eq!(answer["applied"], json!(true));
    assert_eq!(answer["payload"]["answers"], json!([["Fast"]]));
}

#[test]
fn control_requests_open_model_theme_and_palette_surfaces() {
    let mut state = TuiState::new();
    let models = state.apply_control_request(&json!({
        "path": "/tui/open-models",
        "body": {"models": [{"id": "gpt-test", "name": "GPT Test"}]}
    }));
    assert_eq!(models["applied"], json!(true));
    assert_eq!(state.status, "model picker");
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("gpt-test"))
    );

    let theme = state.apply_control_request(&json!({
        "path": "/tui/select-theme",
        "body": {"theme": "midnight"}
    }));
    assert_eq!(theme["applied"], json!(true));
    assert_eq!(state.config.theme, "midnight");

    let themes = state.apply_control_request(&json!({
        "path": "/tui/open-themes",
        "body": {"themes": ["default", "midnight", "high-contrast"]}
    }));
    assert_eq!(themes["applied"], json!(true));
    assert_eq!(state.status, "theme picker");
    let picker = state.choice_picker.as_ref().expect("theme picker");
    assert_eq!(picker.kind, ChoicePickerKind::Theme);
    assert_eq!(
        picker.candidates,
        vec![
            "default".to_string(),
            "midnight".to_string(),
            "high-contrast".to_string()
        ]
    );

    let schemes = state.apply_control_request(&json!({
        "path": "/tui/open-theme-schemes",
        "body": {}
    }));
    assert_eq!(schemes["applied"], json!(true));
    assert_eq!(state.status, "theme-scheme picker");
    let picker = state.choice_picker.as_ref().expect("scheme picker");
    assert_eq!(picker.kind, ChoicePickerKind::ThemeScheme);
    assert_eq!(picker.candidates, default_color_scheme_names());

    let selected_scheme = state.apply_control_request(&json!({
        "path": "/tui/select-theme-scheme",
        "body": {"scheme": "dark"}
    }));
    assert_eq!(selected_scheme["applied"], json!(true));
    assert_eq!(state.config.color_scheme, "dark");

    let cycled_scheme = state.apply_control_request(&json!({
        "path": "/tui/cycle-theme-scheme",
        "body": {}
    }));
    assert_eq!(cycled_scheme["applied"], json!(true));
    assert_eq!(state.config.color_scheme, "system");

    let published_scheme = state.apply_control_request(&json!({
        "path": "/tui/publish",
        "body": {"type": "theme.scheme.light"}
    }));
    assert_eq!(published_scheme["applied"], json!(true));
    assert_eq!(state.config.color_scheme, "light");

    let palette = state.apply_control_request(&json!({
        "path": "/tui/open-palette",
        "body": {"query": "model"}
    }));
    assert_eq!(palette["applied"], json!(true));
    assert_eq!(state.status, "palette open");
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("/models"))
    );

    let agents = state.apply_control_request(&json!({
        "path": "/tui/open-agents",
        "body": {"agents": [{"id": "reviewer", "name": "Reviewer", "description": "Review code"}]}
    }));
    assert_eq!(agents["applied"], json!(true));
    assert_eq!(state.status, "agent picker");
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("reviewer"))
    );

    let variants = state.apply_control_request(&json!({
        "path": "/tui/open-variants",
        "body": {"variants": ["default", "deep"]}
    }));
    assert_eq!(variants["applied"], json!(true));
    assert_eq!(state.status, "variant picker");
    assert!(
        variants["variants"]
            .as_array()
            .is_some_and(|items| items.len() == 2)
    );

    let thinking = state.apply_control_request(&json!({
        "path": "/tui/open-thinking",
        "body": {"levels": ["low", "high"]}
    }));
    assert_eq!(thinking["applied"], json!(true));
    assert_eq!(state.status, "thinking picker");
    assert!(
        thinking["levels"]
            .as_array()
            .is_some_and(|items| items.len() == 2)
    );
}

#[test]
fn remote_control_select_model_dispatches_handler_command() {
    #[derive(Default)]
    struct CaptureHandler {
        commands: Vec<String>,
        responses: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                "command dispatched",
                true,
            )])
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/select-model", "body": {"model": "gpt-test"}}),
    );

    assert_eq!(handler.commands, vec!["/models gpt-test".to_string()]);
    assert_eq!(handler.responses.len(), 1);
    assert_eq!(handler.responses[0]["ok"], json!(true));
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("command dispatched"))
    );
}

#[test]
fn remote_control_open_models_dispatches_picker_fetch() {
    #[derive(Default)]
    struct CaptureHandler {
        model_fetches: usize,
        responses: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn list_models(&mut self) -> Result<Value, String> {
            self.model_fetches += 1;
            Ok(json!({
                "models": [
                    {"id": "server-local", "provider_id": "openagent", "name": "Server Local"},
                    {"id": "deep-model", "provider_id": "openagent", "name": "Deep Model"}
                ]
            }))
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/open-models", "body": {}}),
    );

    assert_eq!(handler.model_fetches, 1);
    assert_eq!(
        state
            .model_picker
            .as_ref()
            .expect("model picker")
            .candidates
            .len(),
        2
    );
    assert_eq!(handler.responses.len(), 1);
    assert_eq!(handler.responses[0]["ok"], json!(true));
}

#[test]
fn remote_control_open_agents_dispatches_picker_fetch() {
    #[derive(Default)]
    struct CaptureHandler {
        agent_fetches: usize,
        responses: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn list_agents(&mut self) -> Result<Value, String> {
            self.agent_fetches += 1;
            Ok(json!({
                "agents": [
                    {"id": "server", "name": "Server", "description": "Default server agent"},
                    {"id": "reviewer", "name": "Reviewer", "description": "Review code"}
                ]
            }))
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/open-agents", "body": {}}),
    );

    assert_eq!(handler.agent_fetches, 1);
    assert_eq!(
        state
            .agent_picker
            .as_ref()
            .expect("agent picker")
            .candidates
            .len(),
        2
    );
    assert_eq!(handler.responses.len(), 1);
    assert_eq!(handler.responses[0]["ok"], json!(true));
}

#[test]
fn remote_control_open_variant_and_thinking_dispatch_picker_fetch() {
    #[derive(Default)]
    struct CaptureHandler {
        model_fetches: usize,
        responses: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn list_models(&mut self) -> Result<Value, String> {
            self.model_fetches += 1;
            Ok(json!({
                "models": [],
                "variants": ["default", "deep"],
                "thinking": ["low", "high"]
            }))
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/open-variants", "body": {}}),
    );

    assert_eq!(handler.model_fetches, 1);
    let picker = state.choice_picker.as_ref().expect("variant picker");
    assert_eq!(picker.kind, ChoicePickerKind::Variant);
    assert_eq!(
        picker.candidates,
        vec!["default".to_string(), "deep".to_string()]
    );
    assert_eq!(handler.responses.len(), 1);
    assert_eq!(handler.responses[0]["ok"], json!(true));

    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/open-thinking", "body": {}}),
    );

    assert_eq!(handler.model_fetches, 2);
    let picker = state.choice_picker.as_ref().expect("thinking picker");
    assert_eq!(picker.kind, ChoicePickerKind::Thinking);
    assert_eq!(
        picker.candidates,
        vec!["low".to_string(), "high".to_string()]
    );
    assert_eq!(handler.responses.len(), 2);
    assert_eq!(handler.responses[1]["ok"], json!(true));
}

#[test]
fn remote_control_agent_variant_and_thinking_dispatch_handler_commands() {
    #[derive(Default)]
    struct CaptureHandler {
        commands: Vec<String>,
        responses: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    for request in [
        json!({"path": "/tui/select-agent", "body": {"agent": "reviewer"}}),
        json!({"path": "/tui/select-variant", "body": {"variant": "deep"}}),
        json!({"path": "/tui/select-thinking", "body": {"level": "high"}}),
        json!({"path": "/tui/publish", "body": {"type": "tui.agent.select", "properties": {"id": "server"}}}),
        json!({"path": "/tui/publish", "body": {"type": "tui.variant.select", "properties": {"value": "fast"}}}),
        json!({"path": "/tui/publish", "body": {"type": "tui.thinking.select", "properties": {"value": "low"}}}),
    ] {
        handle_remote_control_request(&mut state, &mut handler, &request);
    }

    assert_eq!(
        handler.commands,
        vec![
            "/agent reviewer",
            "/variant deep",
            "/thinking high",
            "/agent server",
            "/variant fast",
            "/thinking low",
        ]
    );
    assert_eq!(handler.responses.len(), handler.commands.len());
    assert!(
        handler
            .responses
            .iter()
            .all(|payload| payload["ok"] == json!(true))
    );
}

#[test]
fn remote_control_file_picker_dispatches_and_selects_into_composer() {
    #[derive(Default)]
    struct CaptureHandler {
        commands: Vec<String>,
        responses: Vec<Value>,
        searches: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn search_files(&mut self, query: &str) -> Result<Vec<ComposerFileCandidate>, String> {
            self.searches.push(query.to_string());
            Ok(vec![
                ComposerFileCandidate {
                    reference: "@src/core.rs".to_string(),
                    kind: "file".to_string(),
                },
                ComposerFileCandidate {
                    reference: "@src/main.rs".to_string(),
                    kind: "file".to_string(),
                },
            ])
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/open-files", "body": {"query": "main"}}),
    );
    assert!(handler.commands.is_empty());
    assert_eq!(handler.searches, vec!["main".to_string()]);
    assert_eq!(
        state
            .file_picker
            .as_ref()
            .expect("file picker")
            .candidates
            .len(),
        2
    );

    state.input_buffer = "review".to_string();
    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/publish", "body": {"type": "tui.file.select", "properties": {"path": "src/main.rs", "line": 7}}}),
    );

    assert!(handler.commands.is_empty());
    assert!(state.file_picker.is_none());
    assert_eq!(state.input_buffer, "review @src/main.rs:7 ");
    assert_eq!(handler.responses.len(), 2);
    assert!(
        handler
            .responses
            .iter()
            .all(|payload| payload["ok"] == json!(true))
    );
}

#[test]
fn remote_control_open_sessions_dispatches_picker_search() {
    #[derive(Default)]
    struct CaptureHandler {
        searches: Vec<String>,
        responses: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn search_sessions(&mut self, query: &str) -> Result<Vec<Value>, String> {
            self.searches.push(query.to_string());
            Ok(vec![json!({
                "session_id": "session_alpha",
                "title": "Alpha",
                "status": "idle",
                "message_count": 2,
                "workspace": "/tmp/work"
            })])
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    handle_remote_control_request(
        &mut state,
        &mut handler,
        &json!({"path": "/tui/open-sessions", "body": {"query": "alp"}}),
    );

    assert_eq!(handler.searches, vec!["alp".to_string()]);
    assert_eq!(
        state
            .session_picker
            .as_ref()
            .expect("session picker")
            .candidates
            .len(),
        1
    );
    assert_eq!(handler.responses.len(), 1);
    assert_eq!(handler.responses[0]["ok"], json!(true));
}

#[test]
fn remote_control_session_actions_dispatch_handler_commands() {
    #[derive(Default)]
    struct CaptureHandler {
        commands: Vec<String>,
        responses: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
            self.responses.push(payload.clone());
            Ok(())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    for request in [
        json!({"path": "/tui/select-session", "body": {"sessionID": "session_existing"}}),
        json!({"path": "/tui/rename-session", "body": {"title": "New title"}}),
        json!({"path": "/tui/archive-session", "body": {}}),
        json!({"path": "/tui/unarchive-session", "body": {}}),
        json!({"path": "/tui/fork-session", "body": {}}),
        json!({"path": "/tui/session-children", "body": {}}),
        json!({"path": "/tui/share-session", "body": {}}),
        json!({"path": "/tui/unshare-session", "body": {}}),
        json!({"path": "/tui/compact-session", "body": {}}),
        json!({"path": "/tui/session-details", "body": {}}),
        json!({"path": "/tui/undo-session", "body": {}}),
        json!({"path": "/tui/redo-session", "body": {}}),
        json!({"path": "/tui/publish", "body": {"type": "tui.session.delete"}}),
    ] {
        handle_remote_control_request(&mut state, &mut handler, &request);
    }

    assert_eq!(
        handler.commands,
        vec![
            "/resume session_existing",
            "/rename New title",
            "/archive",
            "/unarchive",
            "/fork",
            "/children",
            "/share",
            "/unshare",
            "/compact",
            "/details",
            "/undo",
            "/redo",
            "/delete",
        ]
    );
    assert_eq!(handler.responses.len(), handler.commands.len());
    assert!(
        handler
            .responses
            .iter()
            .all(|payload| payload["ok"] == json!(true))
    );
}

#[test]
fn local_interaction_commands_are_forwarded_to_terminal_handler() {
    #[derive(Default)]
    struct CaptureHandler {
        approvals: Vec<Value>,
        questions: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_approval_response(
            &mut self,
            payload: &Value,
        ) -> Result<Vec<TimelineLine>, String> {
            self.approvals.push(payload.clone());
            Ok(Vec::new())
        }

        fn handle_question_response(
            &mut self,
            payload: &Value,
        ) -> Result<Vec<TimelineLine>, String> {
            self.questions.push(payload.clone());
            Ok(Vec::new())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    state.apply_app_event(&json!({
        "method": "turn/approval_requested",
        "params": {
            "session_id": "session_1",
            "turn_id": "turn_1",
            "approval": {
                "request_id": "approval_4",
                "turn_id": "turn_1",
                "session_id": "session_1",
                "tool_name": "bash"
            }
        }
    }));

    assert!(
        handle_local_state_command("/allow always", &mut state, &mut handler)
            .expect("allow command should be handled")
    );

    assert_eq!(handler.approvals.len(), 1);
    assert_eq!(handler.approvals[0]["request_id"], json!("approval_4"));
    assert_eq!(handler.approvals[0]["turn_id"], json!("turn_1"));
    assert_eq!(handler.approvals[0]["action"], json!("allow"));
    assert_eq!(handler.approvals[0]["scope"], json!("always"));

    state.apply_app_event(&json!({
        "method": "item/question/requested",
        "params": {
            "event": {
                "request_id": "question_4",
                "turn_id": "turn_1",
                "session_id": "session_1",
                "questions": [{"question": "Mode?"}]
            }
        }
    }));

    assert!(
        handle_local_state_command("/answer Safe path", &mut state, &mut handler)
            .expect("answer command should be handled")
    );

    assert_eq!(handler.questions.len(), 1);
    assert_eq!(handler.questions[0]["request_id"], json!("question_4"));
    assert_eq!(handler.questions[0]["turn_id"], json!("turn_1"));
    assert_eq!(handler.questions[0]["answers"], json!([["Safe path"]]));
}

#[test]
fn composer_history_and_stash_round_trip() {
    let mut state = TuiState::new();
    state.remember_history("first prompt");
    state.remember_history("second prompt");

    state.history_previous();
    assert_eq!(state.input_buffer, "second prompt");
    state.history_previous();
    assert_eq!(state.input_buffer, "first prompt");
    state.history_next();
    assert_eq!(state.input_buffer, "second prompt");
    state.history_next();
    assert_eq!(state.input_buffer, "");

    state.input_buffer = "draft body".to_string();
    state.stash_current_input();
    assert_eq!(state.input_buffer, "");
    assert_eq!(state.stash.len(), 1);
    state.restore_latest_stash();
    assert_eq!(state.input_buffer, "draft body");

    state.input_buffer = "/stash queued draft".to_string();
    assert!(!state.submit());
    assert_eq!(state.stash.last().map(String::as_str), Some("queued draft"));
    state.input_buffer = "/unstash".to_string();
    assert!(!state.submit());
    assert_eq!(state.input_buffer, "queued draft");
}

#[test]
fn composer_expands_file_line_ranges_and_image_attachments() {
    let root = std::env::temp_dir().join(format!("openagent-tui-composer-{}", std::process::id()));
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("main.rs"), "line1\nline2\nline3\nline4\n").expect("write source");
    fs::write(root.join("logo.png"), [0_u8, 1, 2, 3]).expect("write image");

    let expanded = expand_file_attachments(&root, "review @main.rs:2-3 and @logo.png");

    assert!(expanded.prompt.contains("Attached file: src/main.rs:2-3"));
    assert!(expanded.prompt.contains("line2\nline3"));
    assert!(!expanded.prompt.contains("line1\n"));
    assert!(expanded.prompt.contains("Attached image: logo.png"));
    assert_eq!(expanded.lines.len(), 2);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn composer_file_picker_and_attach_controls_insert_references() {
    let root =
        std::env::temp_dir().join(format!("openagent-tui-file-picker-{}", std::process::id()));
    let src = root.join("src");
    let docs = root.join("docs");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&docs).expect("create docs");
    fs::write(src.join("main.rs"), "fn main() {}\n").expect("write main");
    fs::write(docs.join("guide.md"), "guide\n").expect("write guide");
    fs::write(root.join("logo.png"), [0_u8, 1, 2, 3]).expect("write image");

    let matches = fuzzy_find_files(&root, "main", 10);
    assert_eq!(
        matches.first().map(|item| item.reference.as_str()),
        Some("@src/main.rs")
    );
    let lines = file_picker_lines("main", &matches);
    assert!(lines.iter().any(|line| line.text.contains("@src/main.rs")));

    let mut state = TuiState::new();
    state.input_buffer = "/attach src/main.rs:2-3".to_string();
    assert!(!state.submit());
    assert_eq!(state.input_buffer, "@src/main.rs:2-3 ");

    state.input_buffer = "review".to_string();
    let selected = state.apply_control_request(&json!({
        "path": "/tui/select-file",
        "body": {"path": "docs/guide.md", "start": 4, "end": 6}
    }));
    assert_eq!(selected["applied"], json!(true));
    assert_eq!(selected["reference"], json!("@docs/guide.md:4-6"));
    assert_eq!(state.input_buffer, "review @docs/guide.md:4-6 ");

    let image = state.apply_control_request(&json!({
        "path": "/tui/publish",
        "body": {"type": "tui.file.attach", "properties": {"path": "logo.png"}}
    }));
    assert_eq!(image["applied"], json!(true));
    assert!(state.input_buffer.ends_with("@logo.png "));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn tui_config_loads_jsonc_and_theme_command_updates_state() {
    let root = std::env::temp_dir().join(format!("openagent-tui-config-{}", std::process::id()));
    let config_dir = root.join(".openagent");
    fs::create_dir_all(&config_dir).expect("create config dir");
    fs::write(
        config_dir.join("tui.jsonc"),
        r#"{
                // user theme
                "theme": "midnight",
                "color_scheme": "dark",
                "keybinds": {"stash": "ctrl+g"},
                "leader_key": ",",
                "mouse": false,
                "scroll": 9,
                "diff_style": "split",
                "attention_notifications": false,
                "sounds": true
            }"#,
    )
    .expect("write config");

    let config = TuiConfig::load_from_workspace(&root);
    assert_eq!(config.theme, "midnight");
    assert_eq!(config.color_scheme, "dark");
    assert_eq!(config.keybinds["stash"], "ctrl+g");
    assert_eq!(config.leader_key, ",");
    assert!(!config.mouse);
    assert_eq!(config.scroll, 9);
    assert_eq!(config.diff_style, "split");
    assert!(!config.attention_notifications);
    assert!(config.sounds);

    let mut state = TuiState::with_config(config);
    state.input_buffer = "/themes high-contrast".to_string();
    assert!(!state.submit());
    assert_eq!(state.config.theme, "high-contrast");

    state.input_buffer = "/theme-scheme light".to_string();
    assert!(!state.submit());
    assert_eq!(state.config.color_scheme, "light");

    state.input_buffer = "/theme-scheme cycle".to_string();
    assert!(!state.submit());
    assert_eq!(state.config.color_scheme, "dark");

    state.input_buffer = "/config".to_string();
    assert!(!state.submit());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("diff_style"))
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn observability_usage_warnings_and_tool_details_render() {
    let mut state = TuiState::new();
    state.apply_app_event(&json!({
        "method": "turn/completed",
        "params": {
            "status": "completed",
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "cost": 0.01},
            "trace": {"run_id": "turn_1", "model": "server-local"}
        }
    }));
    state.apply_app_event(&json!({
        "method": "turn/completed",
        "params": {
            "status": "completed",
            "usage": {"input_tokens": 2, "output_tokens": 3, "total_tokens": 5, "cost": 0.02}
        }
    }));
    assert_eq!(state.usage_totals["input_tokens"], json!(12));
    assert_eq!(state.usage_totals["output_tokens"], json!(8));
    assert_eq!(state.usage_totals["total_tokens"], json!(20));

    state.apply_app_event(&json!({
        "method": "runtime/warning",
        "params": {"message": "provider throttled"}
    }));
    assert_eq!(
        state.runtime_warnings,
        vec!["provider throttled".to_string()]
    );

    state.input_buffer = "/tool-details on".to_string();
    assert!(!state.submit());
    assert!(state.show_tool_details);
    state.apply_app_event(&json!({
        "method": "item/toolCall/completed",
        "params": {
            "name": "bash",
            "output": "ok",
            "metadata": {"returncode": 0}
        }
    }));
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("tool details"))
    );

    state.input_buffer = "/warnings".to_string();
    assert!(!state.submit());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("provider throttled"))
    );
}

#[test]
fn key_event_flow_submits_prompt_and_uses_history() {
    #[derive(Default)]
    struct CaptureHandler {
        prompts: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, prompt: &str) -> Result<Vec<TimelineLine>, String> {
            self.prompts.push(prompt.to_string());
            Ok(vec![TimelineLine::new("status", "submitted", true)])
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    for ch in "hello".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            &mut state,
            &mut handler,
        )
        .expect("char event");
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("enter event");
    assert_eq!(handler.prompts, vec!["hello".to_string()]);

    handle_key_event(
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("history up");
    assert_eq!(state.input_buffer, "hello");
    handle_key_event(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("history down");
    assert_eq!(state.input_buffer, "");
}

#[test]
fn key_event_flow_opens_file_picker_filters_and_attaches() {
    #[derive(Default)]
    struct CaptureHandler {
        searches: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn search_files(&mut self, query: &str) -> Result<Vec<ComposerFileCandidate>, String> {
            self.searches.push(query.to_string());
            let candidates = match query {
                "ma" => vec![
                    ComposerFileCandidate {
                        reference: "@src/main.rs".to_string(),
                        kind: "file".to_string(),
                    },
                    ComposerFileCandidate {
                        reference: "@images/map.png".to_string(),
                        kind: "image".to_string(),
                    },
                ],
                "mam" => vec![ComposerFileCandidate {
                    reference: "@src/main.rs".to_string(),
                    kind: "file".to_string(),
                }],
                _ => vec![
                    ComposerFileCandidate {
                        reference: "@src/core.rs".to_string(),
                        kind: "file".to_string(),
                    },
                    ComposerFileCandidate {
                        reference: "@docs/guide.md".to_string(),
                        kind: "file".to_string(),
                    },
                ],
            };
            Ok(candidates)
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    for ch in "/files ma".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            &mut state,
            &mut handler,
        )
        .expect("char event");
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("open picker");
    assert_eq!(handler.searches, vec!["ma".to_string()]);
    assert_eq!(
        state.file_picker.as_ref().expect("picker").query.as_str(),
        "ma"
    );

    handle_key_event(
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("filter picker");
    assert_eq!(
        state.file_picker.as_ref().expect("picker").query.as_str(),
        "mam"
    );
    assert_eq!(handler.searches, vec!["ma".to_string(), "mam".to_string()]);

    handle_key_event(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("backspace filter");
    handle_key_event(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("select image");
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("attach selected file");

    assert!(state.file_picker.is_none());
    assert_eq!(state.input_buffer, "@images/map.png ");
}

#[test]
fn key_event_flow_opens_session_picker_filters_and_resumes() {
    #[derive(Default)]
    struct CaptureHandler {
        searches: Vec<String>,
        commands: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn search_sessions(&mut self, query: &str) -> Result<Vec<Value>, String> {
            self.searches.push(query.to_string());
            let sessions = match query {
                "al" => vec![
                    json!({
                        "session_id": "session_alpha",
                        "title": "Alpha",
                        "status": "idle",
                        "message_count": 2,
                        "workspace": "/tmp/alpha"
                    }),
                    json!({
                        "session_id": "session_alpine",
                        "title": "Alpine",
                        "status": "idle",
                        "message_count": 3,
                        "workspace": "/tmp/alpine"
                    }),
                ],
                "alp" => vec![json!({
                    "session_id": "session_alpha",
                    "title": "Alpha",
                    "status": "idle",
                    "message_count": 2,
                    "workspace": "/tmp/alpha"
                })],
                _ => Vec::new(),
            };
            Ok(sessions)
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    send_key_text("/sessions al", &mut state, &mut handler).expect("type sessions command");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("open session picker");

    assert_eq!(handler.searches, vec!["al".to_string()]);
    assert_eq!(
        state
            .session_picker
            .as_ref()
            .expect("session picker")
            .candidates
            .len(),
        2
    );

    press_key(KeyCode::Char('p'), &mut state, &mut handler).expect("filter picker");
    assert_eq!(handler.searches, vec!["al".to_string(), "alp".to_string()]);
    assert_eq!(
        state.session_picker.as_ref().expect("session picker").query,
        "alp"
    );

    press_key(KeyCode::Enter, &mut state, &mut handler).expect("resume selected session");

    assert!(state.session_picker.is_none());
    assert_eq!(state.session_id.as_deref(), Some("session_alpha"));
    assert_eq!(handler.commands, vec!["/resume session_alpha".to_string()]);
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("handled /resume session_alpha"))
    );
}

#[test]
fn key_event_flow_session_picker_manages_sessions_interactively() {
    struct CaptureHandler {
        current: Option<String>,
        searches: Vec<String>,
        commands: Vec<String>,
        sessions: Vec<Value>,
    }

    impl Default for CaptureHandler {
        fn default() -> Self {
            Self {
                current: None,
                searches: Vec::new(),
                commands: Vec::new(),
                sessions: vec![
                    json!({
                        "session_id": "session_alpha",
                        "title": "Alpha",
                        "status": "idle",
                        "message_count": 2,
                        "workspace": "/tmp/alpha",
                        "child_count": 1
                    }),
                    json!({
                        "session_id": "session_child",
                        "title": "Alpha child",
                        "status": "idle",
                        "message_count": 1,
                        "workspace": "/tmp/alpha",
                        "metadata": {"parent_session_id": "session_alpha"}
                    }),
                ],
            }
        }
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            if let Some(session_id) = command.strip_prefix("/resume ") {
                self.current = Some(session_id.to_string());
            } else if let Some(title) = command.strip_prefix("/rename ") {
                if let Some(current) = self.current.as_deref()
                    && let Some(session) = self.sessions.iter_mut().find(|session| {
                        session_id_from_payload(session).as_deref() == Some(current)
                    })
                {
                    session["title"] = json!(title);
                }
            } else if command == "/archive" {
                if let Some(current) = self.current.as_deref()
                    && let Some(session) = self.sessions.iter_mut().find(|session| {
                        session_id_from_payload(session).as_deref() == Some(current)
                    })
                {
                    session["archived"] = json!(true);
                }
            } else if command == "/delete"
                && let Some(current) = self.current.as_deref()
            {
                self.sessions
                    .retain(|session| session_id_from_payload(session).as_deref() != Some(current));
                self.current = None;
            } else if command == "/parent" {
                self.current = Some("session_alpha".to_string());
            }
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn search_sessions(&mut self, query: &str) -> Result<Vec<Value>, String> {
            self.searches.push(query.to_string());
            Ok(self.sessions.clone())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    open_session_picker_from_handler(&mut state, &mut handler, "").expect("open sessions");

    handle_key_event(
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        &mut state,
        &mut handler,
    )
    .expect("show details");
    assert_eq!(
        state.session_picker.as_ref().expect("session picker").mode,
        SessionPickerMode::Details
    );

    press_key(KeyCode::Right, &mut state, &mut handler).expect("open actions");
    press_key(KeyCode::Down, &mut state, &mut handler).expect("details action");
    press_key(KeyCode::Down, &mut state, &mut handler).expect("rename action");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("start rename");
    for _ in 0..5 {
        press_key(KeyCode::Backspace, &mut state, &mut handler).expect("clear title");
    }
    send_key_text("Alpha Prime", &mut state, &mut handler).expect("type new title");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("rename session");

    assert_eq!(
        handler.commands,
        vec![
            "/resume session_alpha".to_string(),
            "/rename Alpha Prime".to_string(),
        ]
    );
    assert_eq!(
        state
            .session_picker
            .as_ref()
            .expect("session picker")
            .candidates[0]["title"],
        json!("Alpha Prime")
    );

    press_key(KeyCode::Right, &mut state, &mut handler).expect("open actions again");
    for _ in 0..3 {
        press_key(KeyCode::Down, &mut state, &mut handler).expect("archive action");
    }
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("confirm archive");
    assert!(matches!(
        state.session_picker.as_ref().expect("session picker").mode,
        SessionPickerMode::Confirm(SessionPickerAction::Archive)
    ));
    press_key(KeyCode::Char('y'), &mut state, &mut handler).expect("archive session");
    assert!(handler.commands.contains(&"/archive".to_string()));
    assert_eq!(
        state
            .session_picker
            .as_ref()
            .expect("session picker")
            .candidates[0]["archived"],
        json!(true)
    );

    state
        .session_picker
        .as_mut()
        .expect("session picker")
        .selected = 1;
    press_key(KeyCode::Right, &mut state, &mut handler).expect("open child actions");
    for _ in 0..8 {
        press_key(KeyCode::Down, &mut state, &mut handler).expect("parent action");
    }
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("navigate parent");
    assert_eq!(state.session_id.as_deref(), Some("session_alpha"));
    assert!(
        handler
            .commands
            .windows(2)
            .any(|window| { window[0] == "/resume session_child" && window[1] == "/parent" })
    );

    press_key(KeyCode::Delete, &mut state, &mut handler).expect("delete confirm");
    assert!(matches!(
        state.session_picker.as_ref().expect("session picker").mode,
        SessionPickerMode::Confirm(SessionPickerAction::Delete)
    ));
}

#[test]
fn key_event_flow_opens_model_picker_filters_and_selects() {
    #[derive(Default)]
    struct CaptureHandler {
        model_fetches: usize,
        commands: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn list_models(&mut self) -> Result<Value, String> {
            self.model_fetches += 1;
            Ok(json!({
                "models": [
                    {"id": "server-local", "provider_id": "openagent", "name": "Server Local"},
                    {"id": "deep-model", "provider_id": "openagent", "name": "Deep Model"}
                ]
            }))
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    send_key_text("/models", &mut state, &mut handler).expect("type models command");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("open model picker");

    assert_eq!(handler.model_fetches, 1);
    assert_eq!(
        state
            .model_picker
            .as_ref()
            .expect("model picker")
            .candidates
            .len(),
        2
    );

    press_key(KeyCode::Char('d'), &mut state, &mut handler).expect("filter picker");
    assert_eq!(
        state
            .model_picker
            .as_ref()
            .expect("model picker")
            .candidates
            .len(),
        1
    );
    assert_eq!(
        state
            .model_picker
            .as_ref()
            .expect("model picker")
            .candidates[0]["id"],
        json!("deep-model")
    );

    press_key(KeyCode::Enter, &mut state, &mut handler).expect("select model");

    assert!(state.model_picker.is_none());
    assert_eq!(handler.commands, vec!["/models deep-model".to_string()]);
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("handled /models deep-model"))
    );
}

#[test]
fn key_event_flow_opens_agent_picker_filters_and_selects() {
    #[derive(Default)]
    struct CaptureHandler {
        agent_fetches: usize,
        commands: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn list_agents(&mut self) -> Result<Value, String> {
            self.agent_fetches += 1;
            Ok(json!({
                "agents": [
                    {"id": "server", "name": "Server", "description": "Default server agent"},
                    {"id": "reviewer", "name": "Reviewer", "description": "Review code"}
                ]
            }))
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    send_key_text("/agents", &mut state, &mut handler).expect("type agents command");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("open agent picker");

    assert_eq!(handler.agent_fetches, 1);
    assert_eq!(
        state
            .agent_picker
            .as_ref()
            .expect("agent picker")
            .candidates
            .len(),
        2
    );

    send_key_text("rev", &mut state, &mut handler).expect("filter picker");
    assert_eq!(
        state
            .agent_picker
            .as_ref()
            .expect("agent picker")
            .candidates[0]["id"],
        json!("reviewer")
    );

    press_key(KeyCode::Enter, &mut state, &mut handler).expect("select agent");

    assert!(state.agent_picker.is_none());
    assert_eq!(handler.commands, vec!["/agent reviewer".to_string()]);
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("handled /agent reviewer"))
    );
}

#[test]
fn key_event_flow_opens_variant_and_thinking_pickers() {
    #[derive(Default)]
    struct CaptureHandler {
        model_fetches: usize,
        commands: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(vec![TimelineLine::new(
                "status",
                format!("handled {command}"),
                true,
            )])
        }

        fn list_models(&mut self) -> Result<Value, String> {
            self.model_fetches += 1;
            Ok(json!({
                "models": [],
                "variants": ["default", "deep"],
                "thinking": ["low", "high"]
            }))
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();

    send_key_text("/variant", &mut state, &mut handler).expect("type variant command");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("open variant picker");
    assert_eq!(handler.model_fetches, 1);
    assert_eq!(
        state.choice_picker.as_ref().expect("variant picker").kind,
        ChoicePickerKind::Variant
    );

    send_key_text("dee", &mut state, &mut handler).expect("filter variant picker");
    assert_eq!(
        state
            .choice_picker
            .as_ref()
            .expect("variant picker")
            .candidates,
        vec!["deep".to_string()]
    );
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("select variant");

    send_key_text("/thinking", &mut state, &mut handler).expect("type thinking command");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("open thinking picker");
    assert_eq!(handler.model_fetches, 2);
    assert_eq!(
        state.choice_picker.as_ref().expect("thinking picker").kind,
        ChoicePickerKind::Thinking
    );

    send_key_text("hi", &mut state, &mut handler).expect("filter thinking picker");
    assert_eq!(
        state
            .choice_picker
            .as_ref()
            .expect("thinking picker")
            .candidates,
        vec!["high".to_string()]
    );
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("select thinking");

    assert!(state.choice_picker.is_none());
    assert_eq!(
        handler.commands,
        vec!["/variant deep".to_string(), "/thinking high".to_string()]
    );
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("handled /thinking high"))
    );
}

#[test]
fn key_event_flow_opens_theme_picker_filters_and_selects() {
    #[derive(Default)]
    struct CaptureHandler {
        commands: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(Vec::new())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();

    send_key_text("/themes", &mut state, &mut handler).expect("type themes command");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("open theme picker");

    let picker = state.choice_picker.as_ref().expect("theme picker");
    assert_eq!(picker.kind, ChoicePickerKind::Theme);
    assert_eq!(picker.candidates, default_theme_names());

    send_key_text("high", &mut state, &mut handler).expect("filter theme picker");
    assert_eq!(
        state
            .choice_picker
            .as_ref()
            .expect("theme picker")
            .candidates,
        vec!["high-contrast".to_string()]
    );
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("select theme");

    assert!(state.choice_picker.is_none());
    assert_eq!(state.config.theme, "high-contrast");
    assert!(handler.commands.is_empty());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("theme set to high-contrast"))
    );
}

#[test]
fn key_event_flow_opens_color_scheme_picker_filters_and_selects() {
    #[derive(Default)]
    struct CaptureHandler {
        commands: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, command: &str) -> Result<Vec<TimelineLine>, String> {
            self.commands.push(command.to_string());
            Ok(Vec::new())
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();

    send_key_text("/theme-scheme", &mut state, &mut handler).expect("type color scheme command");
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("open color scheme picker");

    let picker = state.choice_picker.as_ref().expect("color scheme picker");
    assert_eq!(picker.kind, ChoicePickerKind::ThemeScheme);
    assert_eq!(picker.candidates, default_color_scheme_names());

    send_key_text("da", &mut state, &mut handler).expect("filter color scheme picker");
    assert_eq!(
        state
            .choice_picker
            .as_ref()
            .expect("color scheme picker")
            .candidates,
        vec!["dark".to_string()]
    );
    press_key(KeyCode::Enter, &mut state, &mut handler).expect("select color scheme");

    assert!(state.choice_picker.is_none());
    assert_eq!(state.config.color_scheme, "dark");
    assert!(handler.commands.is_empty());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("color scheme set to dark"))
    );
}

#[test]
fn key_event_flow_at_opens_file_picker_without_touching_commands() {
    #[derive(Default)]
    struct CaptureHandler {
        searches: Vec<String>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn search_files(&mut self, query: &str) -> Result<Vec<ComposerFileCandidate>, String> {
            self.searches.push(query.to_string());
            Ok(vec![
                ComposerFileCandidate {
                    reference: "@src/main.rs".to_string(),
                    kind: "file".to_string(),
                },
                ComposerFileCandidate {
                    reference: "@docs/guide.md".to_string(),
                    kind: "file".to_string(),
                },
            ])
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    state.input_buffer = "review".to_string();
    handle_key_event(
        KeyEvent::new(KeyCode::Char('@'), KeyModifiers::SHIFT),
        &mut state,
        &mut handler,
    )
    .expect("@ opens file picker");

    assert_eq!(handler.searches, vec!["".to_string()]);
    assert_eq!(state.input_buffer, "review");
    assert_eq!(
        state
            .file_picker
            .as_ref()
            .expect("file picker")
            .candidates
            .len(),
        2
    );

    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("attach first candidate");

    assert!(state.file_picker.is_none());
    assert_eq!(state.input_buffer, "review @src/main.rs ");

    state.input_buffer = "/rename ".to_string();
    handle_key_event(
        KeyEvent::new(KeyCode::Char('@'), KeyModifiers::SHIFT),
        &mut state,
        &mut handler,
    )
    .expect("@ stays literal in commands");
    assert_eq!(state.input_buffer, "/rename @");
    assert_eq!(handler.searches, vec!["".to_string()]);
}

#[test]
fn app_bridge_terminal_keyflow_smoke_uses_real_remote_handler() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-keyflow")?;
    fs::write(workspace.join("notes.txt"), "hello from workspace\n")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    let initial = handler.initial_lines();
    apply_handler_output(&mut state, &mut handler, initial);
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("connected to"))
    );

    send_key_text("/new", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    assert_eq!(handler.current_session(), Some("session_smoke"));
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("created session: session_smoke"))
    );

    send_key_text("hello bridge", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    assert_eq!(state.session_id.as_deref(), Some("session_smoke"));
    assert_eq!(state.current_turn_id.as_deref(), Some("turn_smoke"));
    assert_eq!(state.status, "completed");
    assert_eq!(state.usage_totals["total_tokens"], json!(3));
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.kind == "assistant" && line.text.contains("bridge answer"))
    );

    let polled = handler.poll_app_events().map_err(std::io::Error::other)?;
    apply_app_event_values(&mut state, polled);
    assert!(
        state
            .runtime_warnings
            .iter()
            .any(|warning| warning.contains("bridge smoke warning"))
    );

    let recorded = bridge.requests();
    assert!(recorded.iter().any(|request| request == "GET /api/health"));
    assert!(
        recorded
            .iter()
            .any(|request| request == "GET /api/sessions")
    );
    assert!(
        recorded
            .iter()
            .any(|request| request == "POST /api/sessions")
    );
    assert!(
        recorded
            .iter()
            .any(|request| request == "POST /api/sessions/session_smoke/turns")
    );
    assert!(
        recorded
            .iter()
            .any(|request| request.starts_with("GET /api/events?last_event_id="))
    );
    assert!(
        bridge
            .turn_inputs()
            .iter()
            .any(|input| input == "hello bridge")
    );

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_mcp_lifecycle_controls_remote_bridge() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-mcp")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    send_key_text("/mcp", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    let text = timeline_text(&state);
    assert!(text.contains("local-tools enabled=no"));
    assert!(text.contains("lifecycle=stopped"));

    send_key_text("/mcp start local-tools", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    let text = timeline_text(&state);
    assert!(text.contains("mcp start local-tools"));
    assert!(text.contains("lifecycle=running"));
    assert!(text.contains("pid=4242"));

    send_key_text("/mcp enable local-tools", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    let text = timeline_text(&state);
    assert!(text.contains("local-tools enabled=yes"));
    assert!(text.contains("lifecycle=running"));

    send_key_text("/mcp test local-tools", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    let text = timeline_text(&state);
    assert!(text.contains("tools=1"));
    assert!(text.contains("endpoint=python3 +1"));

    send_key_text("/mcp show local-tools", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    let text = timeline_text(&state);
    assert!(text.contains("mcp server: local-tools"));
    assert!(text.contains("tools: stdio_echo"));

    send_key_text("/mcp stop local-tools", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    let text = timeline_text(&state);
    assert!(text.contains("mcp stop local-tools"));
    assert!(text.contains("lifecycle=stopped"));

    let recorded = bridge.requests();
    for expected in [
        "GET /api/mcp",
        "POST /api/mcp/servers/local-tools/start",
        "PATCH /api/mcp/servers/local-tools",
        "POST /api/mcp/servers/local-tools/test",
        "POST /api/mcp/servers/local-tools/stop",
    ] {
        assert!(
            recorded.iter().any(|request| request == expected),
            "missing request {expected}, got {recorded:?}"
        );
    }

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_transcript_reads_real_session_messages() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-transcript")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        session_id: Some("session_smoke".to_string()),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;

    let lines = handler
        .handle_command("/transcript 2")
        .map_err(std::io::Error::other)?;

    assert!(
        lines
            .iter()
            .any(|line| line.text.contains("transcript: 2 of 3 message(s)"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.kind == "message" && line.text.contains("#1 assistant"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.kind == "message" && line.text.contains("bridge answer"))
    );
    let recorded = bridge.requests();
    assert!(
        recorded
            .iter()
            .any(|request| { request == "GET /api/sessions/session_smoke/messages?limit=2" })
    );

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_session_picker_searches_and_resumes() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-session-picker")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    send_key_text("/sessions smoke", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert_eq!(
        state
            .session_picker
            .as_ref()
            .expect("session picker")
            .candidates
            .len(),
        1
    );
    assert!(
        state
            .session_picker
            .as_ref()
            .expect("session picker")
            .candidates[0]["session_id"]
            == json!("session_smoke")
    );

    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert!(state.session_picker.is_none());
    assert_eq!(state.session_id.as_deref(), Some("session_smoke"));
    assert_eq!(handler.current_session(), Some("session_smoke"));
    let recorded = bridge.requests();
    assert!(
        recorded
            .iter()
            .any(|request| request == "GET /api/sessions?query=smoke")
    );

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_session_picker_manages_real_session_actions() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-session-actions")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    send_key_text("/sessions smoke", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    press_key(KeyCode::Right, &mut state, &mut handler)?;
    press_key(KeyCode::Down, &mut state, &mut handler)?;
    press_key(KeyCode::Down, &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    for _ in 0.."Smoke Session".len() {
        press_key(KeyCode::Backspace, &mut state, &mut handler)?;
    }
    send_key_text("Smoke Renamed", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    press_key(KeyCode::Right, &mut state, &mut handler)?;
    for _ in 0..3 {
        press_key(KeyCode::Down, &mut state, &mut handler)?;
    }
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    assert!(matches!(
        state.session_picker.as_ref().expect("session picker").mode,
        SessionPickerMode::Confirm(SessionPickerAction::Archive)
    ));
    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    let updates = bridge.session_update_payloads();
    assert!(
        updates
            .iter()
            .any(|payload| payload["title"] == json!("Smoke Renamed")),
        "expected rename PATCH payload, got {updates:?}"
    );
    assert!(
        updates
            .iter()
            .any(|payload| payload["archived"] == json!(true)),
        "expected archive PATCH payload, got {updates:?}"
    );
    let recorded = bridge.requests();
    assert!(
        recorded
            .iter()
            .filter(|request| request.as_str() == "PATCH /api/sessions/session_smoke")
            .count()
            >= 2
    );

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_model_picker_fetches_and_sets_model() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-model-picker")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        session_id: Some("session_smoke".to_string()),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    send_key_text("/models", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert_eq!(
        state
            .model_picker
            .as_ref()
            .expect("model picker")
            .candidates
            .len(),
        2
    );

    press_key(KeyCode::Char('d'), &mut state, &mut handler)?;
    assert_eq!(
        state
            .model_picker
            .as_ref()
            .expect("model picker")
            .candidates[0]["id"],
        json!("deep-model")
    );

    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert!(state.model_picker.is_none());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("model set to deep-model"))
    );
    let model_updates = bridge.model_update_payloads();
    assert_eq!(model_updates.len(), 1);
    assert_eq!(model_updates[0]["model"], json!("deep-model"));
    let recorded = bridge.requests();
    assert!(recorded.iter().any(|request| request == "GET /api/models"));
    assert!(
        recorded
            .iter()
            .any(|request| request == "PATCH /api/sessions/session_smoke")
    );

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_agent_picker_fetches_and_sets_agent() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-agent-picker")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        session_id: Some("session_smoke".to_string()),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    send_key_text("/agents", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert_eq!(
        state
            .agent_picker
            .as_ref()
            .expect("agent picker")
            .candidates
            .len(),
        2
    );

    send_key_text("rev", &mut state, &mut handler)?;
    assert_eq!(
        state
            .agent_picker
            .as_ref()
            .expect("agent picker")
            .candidates[0]["id"],
        json!("reviewer")
    );

    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert!(state.agent_picker.is_none());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("agent set to reviewer"))
    );
    let agent_updates = bridge.agent_update_payloads();
    assert_eq!(agent_updates.len(), 1);
    assert_eq!(agent_updates[0]["agent"], json!("reviewer"));
    let recorded = bridge.requests();
    assert!(recorded.iter().any(|request| request == "GET /api/agents"));
    assert!(
        recorded
            .iter()
            .any(|request| request == "PATCH /api/sessions/session_smoke")
    );

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_variant_and_thinking_pickers_fetch_and_set() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-choice-picker")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        session_id: Some("session_smoke".to_string()),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    send_key_text("/variant", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    send_key_text("dee", &mut state, &mut handler)?;
    assert_eq!(
        state
            .choice_picker
            .as_ref()
            .expect("variant picker")
            .candidates,
        vec!["deep".to_string()]
    );
    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert!(state.choice_picker.is_none());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("variant set to deep"))
    );
    let variant_updates = bridge.variant_update_payloads();
    assert_eq!(variant_updates.len(), 1);
    assert_eq!(variant_updates[0]["variant"], json!("deep"));

    send_key_text("/thinking", &mut state, &mut handler)?;
    press_key(KeyCode::Enter, &mut state, &mut handler)?;
    send_key_text("hi", &mut state, &mut handler)?;
    assert_eq!(
        state
            .choice_picker
            .as_ref()
            .expect("thinking picker")
            .candidates,
        vec!["high".to_string()]
    );
    press_key(KeyCode::Enter, &mut state, &mut handler)?;

    assert!(state.choice_picker.is_none());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("thinking set to high"))
    );
    let thinking_updates = bridge.thinking_update_payloads();
    assert_eq!(thinking_updates.len(), 1);
    assert_eq!(thinking_updates[0]["thinking"], json!("high"));
    let recorded = bridge.requests();
    assert_eq!(
        recorded
            .iter()
            .filter(|request| request.as_str() == "GET /api/models")
            .count(),
        2
    );
    assert!(
        recorded
            .iter()
            .any(|request| request == "PATCH /api/sessions/session_smoke")
    );

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn app_bridge_terminal_interaction_keyflow_posts_real_responses() -> Result<(), Box<dyn Error>> {
    let bridge = FakeAppBridge::start()?;
    let workspace = temp_test_dir("openagent-tui-bridge-interactions")?;
    let mut handler = AppBridgeTerminalHandler::connect(AppBridgeTerminalOptions {
        server_url: bridge.server_url.clone(),
        auth: RemoteAuth::bearer("secret"),
        workspace: workspace.clone(),
        ..AppBridgeTerminalOptions::default()
    })
    .map_err(std::io::Error::other)?;
    let mut state = TuiState::new();

    state.apply_app_event(&json!({
        "method": "turn/approval_requested",
        "params": {
            "session_id": "session_smoke",
            "thread_id": "session_smoke",
            "turn_id": "turn_approval",
            "status": "waiting_approval",
            "approval": {
                "request_id": "approval_smoke",
                "turn_id": "turn_approval",
                "session_id": "session_smoke",
                "tool_name": "bash",
                "tool_input": {"command": "printf ok"}
            }
        }
    }));
    press_key(KeyCode::Char('1'), &mut state, &mut handler)?;

    assert!(state.active_approval.is_none());
    assert_eq!(state.status, "completed");
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("approved through bridge"))
    );
    let approvals = bridge.approval_payloads();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0]["action"], json!("allow"));
    assert_eq!(approvals[0]["scope"], json!("once"));
    assert_eq!(approvals[0]["request_id"], json!("approval_smoke"));

    state.apply_app_event(&json!({
        "method": "item/question/requested",
        "params": {
            "session_id": "session_smoke",
            "thread_id": "session_smoke",
            "turn_id": "turn_question",
            "event": {
                "request_id": "question_smoke",
                "turn_id": "turn_question",
                "session_id": "session_smoke",
                "questions": [{
                    "header": "Mode",
                    "question": "Which path?",
                    "options": [
                        {"label": "Fast"},
                        {"label": "Safe"}
                    ]
                }]
            }
        }
    }));
    press_key(KeyCode::Char('2'), &mut state, &mut handler)?;

    assert!(state.active_question.is_none());
    assert_eq!(state.status, "completed");
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("question bridge answer"))
    );
    let questions = bridge.question_payloads();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0]["request_id"], json!("question_smoke"));
    assert_eq!(questions[0]["answers"], json!([["Safe"]]));

    let recorded = bridge.requests();
    assert!(
        recorded
            .iter()
            .any(|request| request == "POST /api/turns/turn_approval/approvals/approval_smoke")
    );
    assert!(recorded.iter().any(|request| {
        request == "POST /api/turns/turn_question/questions/question_smoke/reply"
    }));

    bridge.stop();
    let _ = fs::remove_dir_all(workspace);
    Ok(())
}

#[test]
fn terminal_render_snapshot_contains_permission_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.apply_app_event(&json!({
        "method": "turn/approval_requested",
        "params": {
            "approval": {
                "request_id": "approval_overlay",
                "turn_id": "turn_1",
                "tool_name": "write",
                "tool_input": {"file_path": "src/core.rs"},
                "preview": {
                    "kind": "file",
                    "path": "src/core.rs",
                    "diff": "+changed"
                }
            }
        }
    }));

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Interaction: Approval"));
    assert!(rendered.contains("Allow once"));
    assert!(rendered.contains("Always allow"));
    assert!(rendered.contains("Deny"));
    assert!(rendered.contains("write"));
}

#[test]
fn terminal_render_snapshot_contains_file_picker_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.open_file_picker(
        "main",
        vec![
            ComposerFileCandidate {
                reference: "@src/main.rs".to_string(),
                kind: "file".to_string(),
            },
            ComposerFileCandidate {
                reference: "@images/map.png".to_string(),
                kind: "image".to_string(),
            },
        ],
    );

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Composer: Files"));
    assert!(rendered.contains("Query"));
    assert!(rendered.contains("@src/main.rs"));
    assert!(rendered.contains("@images/map.png"));
}

#[test]
fn terminal_render_snapshot_contains_session_picker_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.open_session_picker(
        "alp",
        vec![
            json!({
                "session_id": "session_alpha",
                "title": "Alpha",
                "status": "idle",
                "message_count": 2,
                "workspace": "/tmp/alpha"
            }),
            json!({
                "session_id": "session_alpine",
                "title": "Alpine",
                "status": "running",
                "message_count": 4,
                "workspace": "/tmp/alpine"
            }),
        ],
    );

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Sessions"));
    assert!(rendered.contains("Query"));
    assert!(rendered.contains("session_alpha"));
    assert!(rendered.contains("Alpine"));
}

#[test]
fn terminal_render_snapshot_contains_session_management_views() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.open_session_picker(
        "alp",
        vec![json!({
            "session_id": "session_child",
            "title": "Alpha child",
            "status": "idle",
            "message_count": 2,
            "workspace": "/tmp/alpha",
            "model": "server-local",
            "agent": "reviewer",
            "shared": true,
            "metadata": {"parent_session_id": "session_alpha"}
        })],
    );
    state.session_picker.as_mut().expect("session picker").mode = SessionPickerMode::Details;

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw details frame");
    let details = format!("{:?}", terminal.backend().buffer());
    assert!(details.contains("Details"));
    assert!(details.contains("parent=session_alpha"));
    assert!(details.contains("share=shared"));

    let picker = state.session_picker.as_mut().expect("session picker");
    picker.mode = SessionPickerMode::Actions;
    picker.action_selected = 3;
    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw actions frame");
    let actions = format!("{:?}", terminal.backend().buffer());
    assert!(actions.contains("Actions"));
    assert!(actions.contains("Archive"));
    assert!(actions.contains("confirm"));

    state.session_picker.as_mut().expect("session picker").mode =
        SessionPickerMode::Confirm(SessionPickerAction::Delete);
    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw confirm frame");
    let confirm = format!("{:?}", terminal.backend().buffer());
    assert!(confirm.contains("Confirm"));
    assert!(confirm.contains("Delete session_child"));
}

#[test]
fn terminal_render_snapshot_contains_model_picker_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.open_model_picker(
        "deep",
        vec![
            json!({
                "id": "server-local",
                "provider_id": "openagent",
                "name": "Server Local",
                "default": true
            }),
            json!({
                "id": "deep-model",
                "provider_id": "openagent",
                "name": "Deep Model"
            }),
        ],
    );

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Models"));
    assert!(rendered.contains("Query"));
    assert!(rendered.contains("deep-model"));
    assert!(rendered.contains("Deep Model"));
}

#[test]
fn terminal_render_snapshot_contains_agent_picker_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.open_agent_picker(
        "rev",
        vec![
            json!({
                "id": "server",
                "name": "Server",
                "description": "Default server agent",
                "default": true
            }),
            json!({
                "id": "reviewer",
                "name": "Reviewer",
                "description": "Review code"
            }),
        ],
    );

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Agents"));
    assert!(rendered.contains("Query"));
    assert!(rendered.contains("reviewer"));
    assert!(rendered.contains("Reviewer"));
}

#[test]
fn terminal_render_snapshot_contains_choice_picker_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.open_choice_picker(
        ChoicePickerKind::Variant,
        "dee",
        vec!["default".to_string(), "deep".to_string()],
    );

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Variants"));
    assert!(rendered.contains("Query"));
    assert!(rendered.contains("deep"));
    assert!(rendered.contains("Enter select"));
}

#[test]
fn terminal_render_snapshot_contains_theme_picker_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.set_theme("midnight");
    state.open_choice_picker(ChoicePickerKind::Theme, "", default_theme_names());

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Themes"));
    assert!(rendered.contains("Query"));
    assert!(rendered.contains("midnight  current"));
    assert!(rendered.contains("high-contrast"));
}

#[test]
fn terminal_render_snapshot_contains_color_scheme_picker_overlay() {
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.set_color_scheme("dark");
    state.open_choice_picker(
        ChoicePickerKind::ThemeScheme,
        "",
        default_color_scheme_names(),
    );

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("Color Schemes"));
    assert!(rendered.contains("Query"));
    assert!(rendered.contains("dark  current"));
    assert!(rendered.contains("system"));
}

#[test]
fn key_event_flow_answers_question_option_from_dock() {
    #[derive(Default)]
    struct CaptureHandler {
        question_payloads: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_question_response(
            &mut self,
            payload: &Value,
        ) -> Result<Vec<TimelineLine>, String> {
            self.question_payloads.push(payload.clone());
            Ok(vec![TimelineLine::new("status", "question sent", true)])
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    state.apply_app_event(&json!({
        "method": "item/question/requested",
        "params": {
            "event": {
                "request_id": "question_dock",
                "turn_id": "turn_1",
                "questions": [{
                    "header": "Plan",
                    "question": "Which option?",
                    "options": [
                        {"label": "Fast path", "description": "Move quickly"},
                        {"label": "Safe path", "description": "Be conservative"}
                    ]
                }]
            }
        }
    }));

    handle_key_event(
        KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("question dock key");

    assert_eq!(handler.question_payloads.len(), 1);
    assert_eq!(
        handler.question_payloads[0]["answers"],
        json!([["Safe path"]])
    );
    assert!(state.active_question.is_none());
    assert!(
        state
            .timeline
            .iter()
            .any(|line| line.text.contains("question sent"))
    );
}

#[test]
fn key_event_flow_approves_from_dock() {
    #[derive(Default)]
    struct CaptureHandler {
        approval_payloads: Vec<Value>,
    }

    impl TerminalEventHandler for CaptureHandler {
        fn handle_submit(&mut self, _prompt: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_command(&mut self, _command: &str) -> Result<Vec<TimelineLine>, String> {
            Ok(Vec::new())
        }

        fn handle_approval_response(
            &mut self,
            payload: &Value,
        ) -> Result<Vec<TimelineLine>, String> {
            self.approval_payloads.push(payload.clone());
            Ok(vec![TimelineLine::new("status", "approval sent", true)])
        }
    }

    let mut state = TuiState::new();
    let mut handler = CaptureHandler::default();
    state.apply_app_event(&json!({
        "method": "turn/approval_requested",
        "params": {
            "approval": {
                "request_id": "approval_dock",
                "turn_id": "turn_1",
                "tool_name": "bash",
                "tool_input": {"command": "printf ok"}
            }
        }
    }));

    handle_key_event(
        KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
        &mut state,
        &mut handler,
    )
    .expect("approval dock key");

    assert_eq!(handler.approval_payloads.len(), 1);
    assert_eq!(handler.approval_payloads[0]["action"], json!("allow"));
    assert_eq!(handler.approval_payloads[0]["scope"], json!("once"));
    assert!(state.active_approval.is_none());
}

#[test]
fn terminal_render_snapshot_contains_core_regions() {
    let backend = TestBackend::new(80, 18);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState::new();
    state.status = "running".to_string();
    state
        .timeline
        .push(TimelineLine::new("user", "> hello", true));
    state
        .timeline
        .push(TimelineLine::new("assistant", "world", true));
    state.input_buffer = "next".to_string();

    terminal
        .draw(|frame| draw_terminal_frame(frame, "OpenAgent", &state))
        .expect("draw frame");
    let rendered = format!("{:?}", terminal.backend().buffer());

    assert!(rendered.contains("App Bridge"));
    assert!(rendered.contains("Timeline"));
    assert!(rendered.contains("Prompt"));
    assert!(rendered.contains("OpenAgent"));
    assert!(rendered.contains("world"));
}

fn send_key_text<H: TerminalEventHandler>(
    text: &str,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), Box<dyn Error>> {
    for ch in text.chars() {
        press_key(KeyCode::Char(ch), state, handler)?;
    }
    Ok(())
}

fn press_key<H: TerminalEventHandler>(
    key: KeyCode,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), Box<dyn Error>> {
    let exit = handle_key_event(KeyEvent::new(key, KeyModifiers::NONE), state, handler)
        .map_err(std::io::Error::other)?;
    assert!(!exit, "test key unexpectedly requested TUI exit");
    Ok(())
}

fn temp_test_dir(prefix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn timeline_text(state: &TuiState) -> String {
    state
        .timeline
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct FakeBridgeState {
    requests: Vec<String>,
    turn_inputs: Vec<String>,
    approval_payloads: Vec<Value>,
    question_payloads: Vec<Value>,
    model_update_payloads: Vec<Value>,
    agent_update_payloads: Vec<Value>,
    variant_update_payloads: Vec<Value>,
    thinking_update_payloads: Vec<Value>,
    session_update_payloads: Vec<Value>,
    mcp_enabled: bool,
    mcp_running: bool,
    mcp_pid: u64,
}

struct FakeAppBridge {
    server_url: String,
    state: Arc<Mutex<FakeBridgeState>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeAppBridge {
    fn start() -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(FakeBridgeState::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_shutdown = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        let _ = handle_fake_bridge_connection(stream, &thread_state);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_error) => break,
                }
            }
        });
        Ok(Self {
            server_url: format!("http://127.0.0.1:{port}"),
            state,
            shutdown,
            handle: Some(handle),
        })
    }

    fn requests(&self) -> Vec<String> {
        self.state.lock().expect("bridge state").requests.clone()
    }

    fn turn_inputs(&self) -> Vec<String> {
        self.state.lock().expect("bridge state").turn_inputs.clone()
    }

    fn approval_payloads(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("bridge state")
            .approval_payloads
            .clone()
    }

    fn question_payloads(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("bridge state")
            .question_payloads
            .clone()
    }

    fn model_update_payloads(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("bridge state")
            .model_update_payloads
            .clone()
    }

    fn agent_update_payloads(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("bridge state")
            .agent_update_payloads
            .clone()
    }

    fn variant_update_payloads(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("bridge state")
            .variant_update_payloads
            .clone()
    }

    fn thinking_update_payloads(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("bridge state")
            .thinking_update_payloads
            .clone()
    }

    fn session_update_payloads(&self) -> Vec<Value> {
        self.state
            .lock()
            .expect("bridge state")
            .session_update_payloads
            .clone()
    }

    fn stop(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.server_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_fake_bridge_connection(
    mut stream: TcpStream,
    state: &Arc<Mutex<FakeBridgeState>>,
) -> Result<(), Box<dyn Error>> {
    let (method, path, body) = read_http_request(&mut stream)?;
    state
        .lock()
        .expect("bridge state")
        .requests
        .push(format!("{method} {path}"));
    match (method.as_str(), path.as_str()) {
        ("GET", "/api/health") => write_json(&mut stream, json!({"ok": true})),
        ("GET", "/api/models") => write_json(&mut stream, fake_models_payload()),
        ("GET", "/api/agents") => write_json(&mut stream, fake_agents_payload()),
        ("GET", "/api/mcp") | ("GET", "/api/mcp?refresh=true") => {
            let payload = {
                let state = state.lock().expect("bridge state");
                fake_mcp_payload(&state)
            };
            write_json(&mut stream, payload)
        }
        ("POST", "/api/mcp/servers/local-tools/start") => {
            let payload = {
                let mut state = state.lock().expect("bridge state");
                state.mcp_running = true;
                if state.mcp_pid == 0 {
                    state.mcp_pid = 4242;
                }
                fake_mcp_payload(&state)
            };
            write_json(&mut stream, payload)
        }
        ("POST", "/api/mcp/servers/local-tools/stop") => {
            let payload = {
                let mut state = state.lock().expect("bridge state");
                state.mcp_running = false;
                fake_mcp_payload(&state)
            };
            write_json(&mut stream, payload)
        }
        ("POST", "/api/mcp/servers/local-tools/restart") => {
            let payload = {
                let mut state = state.lock().expect("bridge state");
                state.mcp_running = true;
                state.mcp_pid = state.mcp_pid.saturating_add(1).max(4243);
                fake_mcp_payload(&state)
            };
            write_json(&mut stream, payload)
        }
        ("POST", "/api/mcp/servers/local-tools/test") => {
            let payload = {
                let state = state.lock().expect("bridge state");
                fake_mcp_payload(&state)
            };
            write_json(&mut stream, payload)
        }
        ("PATCH", "/api/mcp/servers/local-tools") => {
            let payload = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
            let payload = {
                let mut state = state.lock().expect("bridge state");
                state.mcp_enabled = payload
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(state.mcp_enabled);
                fake_mcp_payload(&state)
            };
            write_json(&mut stream, payload)
        }
        ("GET", "/api/sessions") => write_json(&mut stream, json!({"sessions": []})),
        ("GET", "/api/sessions?query=smoke") => {
            write_json(&mut stream, fake_session_search_payload())
        }
        ("PATCH", "/api/sessions/session_smoke") => {
            let payload = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
            state
                .lock()
                .expect("bridge state")
                .session_update_payloads
                .push(payload.clone());
            if payload.get("agent").is_some() {
                state
                    .lock()
                    .expect("bridge state")
                    .agent_update_payloads
                    .push(payload.clone());
            }
            if payload.get("model").is_some() {
                state
                    .lock()
                    .expect("bridge state")
                    .model_update_payloads
                    .push(payload.clone());
            }
            if payload.get("variant").is_some() {
                state
                    .lock()
                    .expect("bridge state")
                    .variant_update_payloads
                    .push(payload.clone());
            }
            if payload.get("thinking").is_some() {
                state
                    .lock()
                    .expect("bridge state")
                    .thinking_update_payloads
                    .push(payload.clone());
            }
            write_json(
                &mut stream,
                json!({
                    "session_id": "session_smoke",
                    "updated": true,
                    "session": {
                        "session_id": "session_smoke",
                        "id": "session_smoke",
                        "status": "idle",
                        "title": payload.get("title").cloned().unwrap_or(json!("Smoke Session")),
                        "archived": payload.get("archived").cloned().unwrap_or(json!(false)),
                        "message_count": 3,
                        "workspace": "/tmp/openagent-smoke",
                        "child_count": 1,
                        "model": payload.get("model").cloned().unwrap_or(Value::Null),
                        "agent": payload.get("agent").cloned().unwrap_or(Value::Null),
                        "variant": payload.get("variant").cloned().unwrap_or(Value::Null),
                        "thinking": payload.get("thinking").cloned().unwrap_or(Value::Null),
                        "metadata": {
                            "model": payload.get("model").cloned().unwrap_or(Value::Null),
                            "agent": payload.get("agent").cloned().unwrap_or(Value::Null),
                            "variant": payload.get("variant").cloned().unwrap_or(Value::Null),
                            "thinking": payload.get("thinking").cloned().unwrap_or(Value::Null)
                        }
                    }
                }),
            )
        }
        ("DELETE", "/api/sessions/session_smoke") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "deleted": true
            }),
        ),
        ("GET", "/api/sessions/session_smoke") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "session": {
                    "session_id": "session_smoke",
                    "title": "Smoke Session",
                    "status": "idle",
                    "metadata": {}
                }
            }),
        ),
        ("GET", "/api/sessions/session_smoke/children") => write_json(
            &mut stream,
            json!({
                "children": [{
                    "session_id": "session_child",
                    "title": "Smoke Child",
                    "status": "idle",
                    "message_count": 1,
                    "workspace": "/tmp/openagent-smoke",
                    "metadata": {"parent_session_id": "session_smoke"}
                }]
            }),
        ),
        ("POST", "/api/sessions/session_smoke/share") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "shared": true,
                "share_url": "https://share.example/session_smoke"
            }),
        ),
        ("DELETE", "/api/sessions/session_smoke/share") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "shared": false
            }),
        ),
        ("POST", "/api/sessions/session_smoke/compact") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "summary": {"content": "compacted smoke session"}
            }),
        ),
        ("GET", "/api/sessions/session_smoke/diff") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "files": [],
                "summary": {"changed": 0}
            }),
        ),
        ("POST", "/api/sessions/session_smoke/undo") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "status": "ok",
                "events": []
            }),
        ),
        ("POST", "/api/sessions/session_smoke/redo") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "status": "ok",
                "events": []
            }),
        ),
        ("GET", "/api/sessions/session_smoke/messages?limit=2") => {
            write_json(&mut stream, fake_transcript_payload())
        }
        ("POST", "/api/sessions") => write_json(
            &mut stream,
            json!({
                "session_id": "session_smoke",
                "session": {
                    "session_id": "session_smoke",
                    "status": "ready",
                    "message_count": 0
                }
            }),
        ),
        ("POST", "/api/sessions/session_smoke/turns") => {
            let input = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("input")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_default();
            state.lock().expect("bridge state").turn_inputs.push(input);
            write_json(
                &mut stream,
                json!({
                    "session_id": "session_smoke",
                    "turn_id": "turn_smoke",
                    "status": "completed",
                    "events": fake_turn_events(),
                }),
            )
        }
        ("POST", "/api/turns/turn_approval/approvals/approval_smoke") => {
            let payload = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
            state
                .lock()
                .expect("bridge state")
                .approval_payloads
                .push(payload);
            write_json(
                &mut stream,
                json!({
                    "session_id": "session_smoke",
                    "turn_id": "turn_approval",
                    "status": "completed",
                    "events": fake_approval_response_events(),
                }),
            )
        }
        ("POST", "/api/turns/turn_question/questions/question_smoke/reply") => {
            let payload = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
            state
                .lock()
                .expect("bridge state")
                .question_payloads
                .push(payload);
            write_json(
                &mut stream,
                json!({
                    "session_id": "session_smoke",
                    "turn_id": "turn_question",
                    "status": "completed",
                    "events": fake_question_response_events(),
                }),
            )
        }
        _ if method == "GET" && path.starts_with("/api/events?last_event_id=") => {
            let last_event_id = path
                .rsplit_once('=')
                .and_then(|(_, value)| value.parse::<u64>().ok())
                .unwrap_or_default();
            let events = if last_event_id < 4 {
                vec![json!({
                    "method": "runtime/warning",
                    "global_sequence": 4,
                    "sequence": 4,
                    "params": {
                        "session_id": "session_smoke",
                        "turn_id": "turn_smoke",
                        "message": "bridge smoke warning"
                    }
                })]
            } else {
                Vec::new()
            };
            write_sse(&mut stream, &events)
        }
        _ => write_response(
            &mut stream,
            "404 Not Found",
            "application/json",
            &json!({"error": format!("unexpected route: {method} {path}")}).to_string(),
        ),
    }?;
    Ok(())
}

fn fake_turn_events() -> Vec<Value> {
    vec![
        json!({
            "method": "turn/started",
            "global_sequence": 1,
            "sequence": 1,
            "params": {
                "session_id": "session_smoke",
                "thread_id": "session_smoke",
                "turn_id": "turn_smoke",
                "status": "running"
            }
        }),
        json!({
            "method": "item/agentMessage/delta",
            "global_sequence": 2,
            "sequence": 2,
            "params": {
                "session_id": "session_smoke",
                "thread_id": "session_smoke",
                "turn_id": "turn_smoke",
                "delta": "bridge answer"
            }
        }),
        json!({
            "method": "turn/completed",
            "global_sequence": 3,
            "sequence": 3,
            "params": {
                "session_id": "session_smoke",
                "thread_id": "session_smoke",
                "turn_id": "turn_smoke",
                "status": "completed",
                "final_answer": "bridge answer",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 2,
                    "total_tokens": 3,
                    "cost": 0.0
                }
            }
        }),
    ]
}

fn fake_models_payload() -> Value {
    json!({
        "models": [
            {
                "id": "server-local",
                "provider_id": "openagent",
                "name": "Server Local",
                "default": true
            },
            {
                "id": "deep-model",
                "provider_id": "openagent",
                "name": "Deep Model"
            }
        ],
        "variants": ["default", "deep"],
        "thinking": ["low", "high"]
    })
}

fn fake_mcp_payload(state: &FakeBridgeState) -> Value {
    let lifecycle_status = if state.mcp_running {
        "running"
    } else {
        "stopped"
    };
    let lifecycle_pid = if state.mcp_running && state.mcp_pid > 0 {
        json!(state.mcp_pid)
    } else {
        Value::Null
    };
    let tool_count = if state.mcp_running { 1 } else { 0 };
    json!({
        "status": "ok",
        "servers": [{
            "name": "local-tools",
            "enabled": state.mcp_enabled,
            "type": "local",
            "transport": "stdio",
            "selected_transport": "stdio",
            "command": "python3",
            "args_count": 1,
            "status": if state.mcp_running { "connected" } else { "configured" },
            "tool_count": tool_count,
            "tools": if state.mcp_running {
                json!([{
                    "name": "stdio_echo",
                    "title": "Stdio Echo",
                    "description": "Echo text"
                }])
            } else {
                json!([])
            },
            "lifecycle_status": lifecycle_status,
            "lifecycle_pid": lifecycle_pid,
            "lifecycle_tool_count": tool_count,
            "remote_url_configured": false
        }]
    })
}

fn fake_agents_payload() -> Value {
    json!({
        "agents": [
            {
                "id": "server",
                "name": "Server",
                "description": "Default server-backed coding agent",
                "default": true
            },
            {
                "id": "reviewer",
                "name": "Reviewer",
                "description": "Review code"
            }
        ]
    })
}

fn fake_session_search_payload() -> Value {
    json!({
        "sessions": [{
            "session_id": "session_smoke",
            "title": "Smoke Session",
            "status": "idle",
            "message_count": 3,
            "workspace": "/tmp/openagent-smoke",
            "child_count": 1,
            "shared": false,
            "archived": false
        }]
    })
}

fn fake_transcript_payload() -> Value {
    json!({
        "session_id": "session_smoke",
        "message_count": 3,
        "limit": 2,
        "messages": [
            {
                "index": 1,
                "role": "assistant",
                "content": "bridge answer",
                "name": null,
                "tool_call_id": null,
                "metadata": {}
            },
            {
                "index": 2,
                "role": "user",
                "content": "next question",
                "name": null,
                "tool_call_id": null,
                "metadata": {}
            }
        ]
    })
}

fn fake_approval_response_events() -> Vec<Value> {
    vec![
        json!({
            "method": "turn/approval_resolved",
            "global_sequence": 10,
            "sequence": 10,
            "params": {
                "session_id": "session_smoke",
                "thread_id": "session_smoke",
                "turn_id": "turn_approval",
                "status": "running",
                "approval": {
                    "request_id": "approval_smoke",
                    "turn_id": "turn_approval",
                    "session_id": "session_smoke",
                    "tool_name": "bash",
                    "action": "allow",
                    "scope": "once"
                }
            }
        }),
        json!({
            "method": "turn/completed",
            "global_sequence": 11,
            "sequence": 11,
            "params": {
                "session_id": "session_smoke",
                "thread_id": "session_smoke",
                "turn_id": "turn_approval",
                "status": "completed",
                "final_answer": "approved through bridge"
            }
        }),
    ]
}

fn fake_question_response_events() -> Vec<Value> {
    vec![
        json!({
            "method": "item/question/resolved",
            "global_sequence": 12,
            "sequence": 12,
            "params": {
                "session_id": "session_smoke",
                "thread_id": "session_smoke",
                "turn_id": "turn_question",
                "status": "answered",
                "question": {
                    "request_id": "question_smoke",
                    "turn_id": "turn_question",
                    "session_id": "session_smoke"
                }
            }
        }),
        json!({
            "method": "turn/completed",
            "global_sequence": 13,
            "sequence": 13,
            "params": {
                "session_id": "session_smoke",
                "thread_id": "session_smoke",
                "turn_id": "turn_question",
                "status": "completed",
                "final_answer": "question bridge answer"
            }
        }),
    ]
}

fn read_http_request(stream: &mut TcpStream) -> Result<(String, String, String), Box<dyn Error>> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 1024];
    let mut expected_len = None;
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => raw.extend_from_slice(&buffer[..count]),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                break;
            }
            Err(error) => return Err(error.into()),
        }
        if expected_len.is_none()
            && let Some(header_end) = find_header_end(&raw)
        {
            let headers = String::from_utf8_lossy(&raw[..header_end]).to_string();
            let content_len = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_default();
            expected_len = Some(header_end + content_len);
        }
        if let Some(expected_len) = expected_len
            && raw.len() >= expected_len
        {
            break;
        }
    }
    let header_end = find_header_end(&raw).ok_or("missing HTTP headers")?;
    let headers = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let mut lines = headers.lines();
    let request_line = lines.next().ok_or("missing request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let body = String::from_utf8_lossy(&raw[header_end..]).to_string();
    Ok((method, path, body))
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn write_json(stream: &mut TcpStream, body: Value) -> Result<(), Box<dyn Error>> {
    write_response(stream, "200 OK", "application/json", &body.to_string())
}

fn write_sse(stream: &mut TcpStream, events: &[Value]) -> Result<(), Box<dyn Error>> {
    let mut body = String::new();
    for event in events {
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("event");
        let id = event
            .get("global_sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        body.push_str(&format!("event: {method}\nid: {id}\ndata: {event}\n\n"));
    }
    write_response(stream, "200 OK", "text/event-stream", &body)
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<(), Box<dyn Error>> {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

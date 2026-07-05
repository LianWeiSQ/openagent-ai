use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use openagent_core::PermissionManager;
use openagent_lsp::command_available;
use openagent_protocol::{PermissionAction, PermissionRuleset, ToolCall, ToolResult, Usage};
use openagent_tools::{
    LocalWorkspaceRuntime, SessionRunnerFacade, TaskSubagentDescriptor, TodoItem, ToolContext,
    ToolRegistry, Toolkit, benchmark_mode_allows_shell_command,
    benchmark_mode_value_allows_shell_command, blocked_command, ensure_within_root,
    exclusive_schema, format_read_output_from_text, parse_agent_profile_schema,
    prepare_isolated_workspace, qualify_tool_id, question_answers_from_json, readonly_schema,
    register_builtin_tools, select_task_subagent_for_prompt, truncate_output,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn tool_runtime_fixture_matches_legacy_oracle() -> Result<(), Box<dyn Error>> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/golden/rust_rewrite/tool_runtime.json"
    ))?;
    assert_eq!(fixture, tool_runtime_fixture()?);
    Ok(())
}

#[test]
fn shared_agent_profile_schema_parses_skill_task_config_without_model_option_leaks() {
    let schema = parse_agent_profile_schema(
        &json!({
            "id": "Skillful Agent",
            "name": "Skillful Agent",
            "description": "Shared parser fixture",
            "mode": "subagent",
            "permission": {
                "ruleset": "READONLY",
                "task": {"reviewer": "allow"},
                "skill": {"hidden": "deny"}
            },
            "skills": "brief, review",
            "skill_roots": ["shared-skills"],
            "skill_permissions": [{"name": "visible", "action": "allow"}],
            "task_permissions": [{"subagent": "planner", "action": "ask"}],
            "tools": ["read", "skill"],
            "steps": 4,
            "temperature": 0.2,
            "topP": 0.8,
            "options": {
                "reasoning_effort": "medium",
                "skill_roots": ["must-not-leak"],
                "task_permissions": {"leaked": "deny"}
            },
            "model_options": {
                "verbosity": "low",
                "skill_permissions": {"leaked": "deny"}
            }
        }),
        "fallback-agent",
        "Fallback Agent",
    )
    .expect("schema parses");

    assert_eq!(schema.id, "skillful-agent");
    assert_eq!(schema.mode, "subagent");
    assert_eq!(schema.permission.as_deref(), Some("READONLY"));
    assert_eq!(schema.skill.skills, vec!["brief", "review"]);
    assert_eq!(schema.skill.roots, vec!["shared-skills"]);
    assert!(
        schema.skill.permissions.iter().any(|rule| {
            rule.pattern == "hidden" && matches!(rule.action, PermissionAction::Deny)
        })
    );
    assert!(schema.skill.permissions.iter().any(|rule| {
        rule.pattern == "visible" && matches!(rule.action, PermissionAction::Allow)
    }));
    assert!(schema.task.permissions.iter().any(|rule| {
        rule.pattern == "reviewer" && matches!(rule.action, PermissionAction::Allow)
    }));
    assert!(
        schema.task.permissions.iter().any(|rule| {
            rule.pattern == "planner" && matches!(rule.action, PermissionAction::Ask)
        })
    );
    assert_eq!(schema.model_options["reasoning_effort"], "medium");
    assert_eq!(schema.model_options["verbosity"], "low");
    assert_eq!(schema.model_options["temperature"], 0.2);
    assert_eq!(schema.model_options["top_p"], 0.8);
    for reserved in [
        "skill_roots",
        "skill_permissions",
        "task_permissions",
        "skills",
    ] {
        assert!(schema.model_options.get(reserved).is_none());
    }

    let error = parse_agent_profile_schema(&json!({"mode": "worker"}), "bad", "Bad")
        .expect_err("invalid mode should fail");
    assert!(error.contains("invalid mode"));
}

#[test]
fn session_runner_facade_builds_shared_tool_context_contract() {
    let mut manager = PermissionManager::new();
    manager.set_ruleset(PermissionRuleset::Readonly);
    let facade = SessionRunnerFacade::new("/tmp/openagent-session-runner", "session_runner")
        .with_agent_options(BTreeMap::from([
            ("agent_id".to_string(), json!("researcher")),
            ("agent".to_string(), json!("researcher")),
            ("skills".to_string(), json!(["brief"])),
            ("skill_roots".to_string(), json!(["shared-skills"])),
            (
                "skill_permissions".to_string(),
                json!([{"pattern": "hidden", "action": "deny"}]),
            ),
        ]))
        .with_permission_manager(manager)
        .with_dangerously_skip_permissions(true)
        .with_question_answers(vec![vec!["yes".to_string(), "ship it".to_string()]]);

    let context = facade.tool_context();
    assert_eq!(context.session_id, "session_runner");
    assert_eq!(context.agent_options["agent_id"], json!("researcher"));
    assert_eq!(context.agent_options["skills"], json!(["brief"]));
    assert_eq!(
        context.agent_options["skill_roots"],
        json!(["shared-skills"])
    );
    assert!(context.permission_manager.is_some());
    assert!(context.dangerously_skip_permissions);
    assert_eq!(
        context.question_answers,
        Some(vec![vec!["yes".to_string(), "ship it".to_string()]])
    );

    let contract = facade.contract_value();
    assert_eq!(contract["session_id"], "session_runner");
    assert_eq!(contract["agent_id"], "researcher");
    assert_eq!(contract["skills"], json!(["brief"]));
    assert_eq!(contract["skill_roots"], json!(["shared-skills"]));
    assert_eq!(contract["has_skill_permissions"], true);
    assert_eq!(contract["has_permission_manager"], true);
    assert_eq!(contract["question_answer_groups"], 1);
}

#[test]
fn session_runner_facade_parses_question_answers_from_json_contract() {
    assert_eq!(
        question_answers_from_json(&json!([["yes", true], [42, 3.5]])).unwrap(),
        vec![
            vec!["yes".to_string(), "true".to_string()],
            vec!["42".to_string(), "3.5".to_string()],
        ]
    );
    assert_eq!(
        question_answers_from_json(&json!(["alpha", false, 7])).unwrap(),
        vec![
            vec!["alpha".to_string()],
            vec!["false".to_string()],
            vec!["7".to_string()],
        ]
    );
    assert!(question_answers_from_json(&json!({"answer": "no"})).is_none());

    let context = SessionRunnerFacade::new("/tmp/openagent-session-runner", "session_answers")
        .with_question_answers_value(&json!([["fast", "careful"]]))
        .tool_context();
    assert_eq!(
        context.question_answers,
        Some(vec![vec!["fast".to_string(), "careful".to_string()]])
    );
}

#[test]
fn session_runner_facade_builds_shared_tool_call_events() {
    let facade = SessionRunnerFacade::new("/tmp/openagent-session-runner", "session_events");
    let call = ToolCall {
        name: "read".to_string(),
        input: json!({"file_path": "README.md"}),
        call_id: "call_read".to_string(),
    };
    let started = facade.tool_call_started_event(
        "turn_1",
        2,
        &call,
        Some("turn_1"),
        BTreeMap::from([("manual".to_string(), json!(true))]),
    );
    assert_eq!(started["method"], "item/toolCall/started");
    assert_eq!(started["params"]["session_id"], "session_events");
    assert_eq!(started["params"]["turn_id"], "turn_1");
    assert_eq!(started["params"]["run_id"], "turn_1");
    assert_eq!(started["params"]["step"], 2);
    assert_eq!(started["params"]["call_id"], "call_read");
    assert_eq!(started["params"]["name"], "read");
    assert_eq!(started["params"]["input"]["file_path"], "README.md");
    assert_eq!(started["params"]["manual"], true);

    let result = ToolResult {
        call_id: "call_read".to_string(),
        output: "ok".to_string(),
        error: None,
        metadata: BTreeMap::from([("bytes".to_string(), json!(12))]),
    };
    let completed =
        facade.tool_call_finished_event("turn_1", 2, &call, &result, None, BTreeMap::new());
    assert_eq!(completed["method"], "item/toolCall/completed");
    assert_eq!(completed["params"]["output"], "ok");
    assert_eq!(completed["params"]["error"], Value::Null);
    assert_eq!(completed["params"]["metadata"]["bytes"], 12);
    assert!(completed["params"].get("turn_id").is_none());

    let failed = facade.tool_call_finished_event(
        "turn_1",
        2,
        &call,
        &ToolResult {
            call_id: "call_read".to_string(),
            output: String::new(),
            error: Some("boom".to_string()),
            metadata: BTreeMap::new(),
        },
        Some("turn_1"),
        BTreeMap::from([("auto".to_string(), json!(true))]),
    );
    assert_eq!(failed["method"], "item/toolCall/failed");
    assert_eq!(failed["params"]["error"], "boom");
    assert_eq!(failed["params"]["auto"], true);
}

#[test]
fn session_runner_facade_builds_shared_tool_result_session_projection() {
    let facade = SessionRunnerFacade::new("/tmp/openagent-session-runner", "session_projection");
    let call = ToolCall {
        name: "skill".to_string(),
        input: json!({"name": "review"}),
        call_id: "call_skill".to_string(),
    };
    let result = ToolResult {
        call_id: "call_skill".to_string(),
        output: "<skill_content name=\"review\">...</skill_content>".to_string(),
        error: None,
        metadata: BTreeMap::from([
            ("skill_name".to_string(), json!("review")),
            (
                "skill_location".to_string(),
                json!("/workspace/skills/review/SKILL.md"),
            ),
            (
                "skill_files".to_string(),
                json!(["references/checklist.md"]),
            ),
        ]),
    };

    let message = facade.tool_result_message(
        3,
        &call,
        &result,
        Some("msg_assistant"),
        Some("msg_tool".to_string()),
    );
    assert_eq!(message.role, openagent_protocol::Role::Tool);
    assert_eq!(message.content, result.output);
    assert_eq!(message.name.as_deref(), Some("skill"));
    assert_eq!(message.tool_call_id.as_deref(), Some("call_skill"));
    assert_eq!(message.metadata["message_id"], "msg_tool");
    assert_eq!(message.metadata["assistant_message_id"], "msg_assistant");
    assert_eq!(message.metadata["step"], 3);
    assert_eq!(message.metadata["tool_result"]["call_id"], "call_skill");

    let projection = facade.tool_result_session_projection(3, &call, &result);
    assert!(!projection.failed);
    assert_eq!(projection.event_name, "tool.call.finished");
    assert_eq!(projection.event_status, "ok");
    assert_eq!(projection.event_attributes["call_id"], "call_skill");
    assert_eq!(projection.event_attributes["name"], "skill");
    assert_eq!(
        projection.event_attributes["metadata"]["skill_name"],
        "review"
    );
    assert_eq!(projection.part_attributes["failed"], false);

    let skill_event = facade
        .skill_tool_session_event(3, &call, &result)
        .expect("skill load event");
    assert_eq!(skill_event.event_name, "skill.loaded");
    assert_eq!(skill_event.attributes["skill_name"], "review");
    assert_eq!(
        skill_event.attributes["skill_location"],
        "/workspace/skills/review/SKILL.md"
    );
    assert_eq!(
        skill_event.attributes["skill_files"][0],
        "references/checklist.md"
    );
    let settlement = facade.tool_result_settlement(
        3,
        &call,
        &result,
        Some("msg_assistant"),
        Some("msg_tool_settlement".to_string()),
    );
    assert!(!settlement.failed);
    assert_eq!(
        settlement.message.metadata["message_id"],
        "msg_tool_settlement"
    );
    assert_eq!(settlement.projection.event_name, "tool.call.finished");
    assert_eq!(
        settlement
            .skill_event
            .as_ref()
            .expect("settled skill event")
            .event_name,
        "skill.loaded"
    );
    assert_eq!(settlement.event_intents.len(), 2);
    assert_eq!(settlement.event_intents[0].event_name, "skill.loaded");
    assert_eq!(settlement.event_intents[0].kind, "skill");
    assert_eq!(settlement.event_intents[0].status, "ok");
    assert_eq!(settlement.event_intents[1].event_name, "tool.call.finished");
    assert_eq!(settlement.event_intents[1].kind, "tool");
    assert_eq!(settlement.event_intents[1].status, "ok");
    assert_eq!(settlement.part_intent.part_type, "tool_result");
    assert_eq!(settlement.part_intent.step_index, Some(3));
    assert_eq!(settlement.part_intent.status, "ok");
    assert_eq!(settlement.part_intent.attributes["failed"], false);

    let failed_result = ToolResult {
        call_id: "call_skill".to_string(),
        output: String::new(),
        error: Some("denied".to_string()),
        metadata: BTreeMap::new(),
    };
    let failed_message = facade.tool_result_message(4, &call, &failed_result, None, None);
    assert_eq!(failed_message.content, "Tool failed: denied");
    let failed_projection = facade.tool_result_session_projection(4, &call, &failed_result);
    assert!(failed_projection.failed);
    assert_eq!(failed_projection.event_name, "tool.call.failed");
    assert_eq!(failed_projection.event_status, "error");
    assert!(
        facade
            .skill_tool_session_event(4, &call, &failed_result)
            .is_none()
    );
    let failed_settlement = facade.tool_result_settlement(4, &call, &failed_result, None, None);
    assert!(failed_settlement.failed);
    assert_eq!(failed_settlement.message.content, "Tool failed: denied");
    assert_eq!(failed_settlement.projection.event_name, "tool.call.failed");
    assert!(failed_settlement.skill_event.is_none());
    assert_eq!(failed_settlement.event_intents.len(), 1);
    assert_eq!(
        failed_settlement.event_intents[0].event_name,
        "tool.call.failed"
    );
    assert_eq!(failed_settlement.event_intents[0].kind, "tool");
    assert_eq!(failed_settlement.event_intents[0].status, "error");
    assert_eq!(failed_settlement.part_intent.attributes["failed"], true);

    let lsp_call = ToolCall {
        name: "lsp".to_string(),
        input: json!({"operation": "documentSymbol", "file_path": "src/main.rs"}),
        call_id: "call_lsp".to_string(),
    };
    let lsp_result = ToolResult {
        call_id: "call_lsp".to_string(),
        output: "{}".to_string(),
        error: None,
        metadata: BTreeMap::from([
            ("operation".to_string(), json!("documentSymbol")),
            ("server_id".to_string(), json!("fake")),
            ("file_path".to_string(), json!("/workspace/src/main.rs")),
            ("diagnostics".to_string(), json!({})),
        ]),
    };
    let lsp_event = facade
        .lsp_tool_session_event(5, &lsp_call, &lsp_result)
        .expect("lsp updated event");
    assert_eq!(lsp_event.event_name, "lsp.updated");
    assert_eq!(lsp_event.kind, "lsp");
    assert_eq!(lsp_event.attributes["operation"], "documentSymbol");
    assert_eq!(lsp_event.attributes["server_id"], "fake");
    let lsp_settlement = facade.tool_result_settlement(5, &lsp_call, &lsp_result, None, None);
    assert_eq!(lsp_settlement.event_intents.len(), 2);
    assert_eq!(lsp_settlement.event_intents[0].event_name, "lsp.updated");
    assert_eq!(
        lsp_settlement.event_intents[1].event_name,
        "tool.call.finished"
    );
}

#[test]
fn session_runner_facade_builds_shared_turn_terminal_events() {
    let facade = SessionRunnerFacade::new("/tmp/openagent-session-runner", "session_turn");

    let cli_completed = facade.turn_terminal_event(
        "turn/completed",
        "run_cli",
        "completed",
        false,
        false,
        true,
        BTreeMap::from([
            ("final_answer".to_string(), json!("done")),
            ("steps".to_string(), json!(2)),
        ]),
    );
    assert_eq!(cli_completed["method"], "turn/completed");
    assert_eq!(cli_completed["params"]["session_id"], "session_turn");
    assert_eq!(cli_completed["params"]["run_id"], "run_cli");
    assert_eq!(cli_completed["params"]["status"], "completed");
    assert_eq!(cli_completed["params"]["final_answer"], "done");
    assert!(cli_completed["params"].get("turn_id").is_none());
    assert!(cli_completed["params"].get("thread_id").is_none());

    let http_failed = facade.turn_terminal_event(
        "turn/failed",
        "turn_http",
        "failed",
        true,
        true,
        false,
        BTreeMap::from([("error".to_string(), json!("provider failed"))]),
    );
    assert_eq!(http_failed["method"], "turn/failed");
    assert_eq!(http_failed["params"]["session_id"], "session_turn");
    assert_eq!(http_failed["params"]["thread_id"], "session_turn");
    assert_eq!(http_failed["params"]["turn_id"], "turn_http");
    assert_eq!(http_failed["params"]["status"], "failed");
    assert_eq!(http_failed["params"]["error"], "provider failed");
    assert!(http_failed["params"].get("run_id").is_none());
}

#[test]
fn session_runner_facade_builds_shared_turn_terminal_outcomes() {
    let completed = SessionRunnerFacade::completed_turn_outcome(0, "stop");
    assert_eq!(completed.event_method, "turn/completed");
    assert_eq!(completed.run_status, "completed");
    assert_eq!(completed.event_status, "completed");
    assert_eq!(completed.steps, 1);
    assert_eq!(completed.finish_reason, "stop");
    assert_eq!(completed.error, None);

    let failed = SessionRunnerFacade::failed_turn_outcome(3, "provider_error", "timeout");
    assert_eq!(failed.event_method, "turn/failed");
    assert_eq!(failed.run_status, "failed");
    assert_eq!(failed.event_status, "failed");
    assert_eq!(failed.steps, 3);
    assert_eq!(failed.finish_reason, "provider_error");
    assert_eq!(failed.error.as_deref(), Some("timeout"));

    let paused = SessionRunnerFacade::paused_turn_outcome(2, "approval_required", "needs approval");
    assert_eq!(paused.event_method, "turn/completed");
    assert_eq!(paused.run_status, "failed");
    assert_eq!(paused.event_status, "paused");
    assert_eq!(paused.steps, 2);
    assert_eq!(paused.finish_reason, "approval_required");
    assert_eq!(paused.error.as_deref(), Some("needs approval"));

    let interrupted = SessionRunnerFacade::interrupted_turn_outcome("cancelled");
    assert_eq!(interrupted.event_method, "turn/interrupted");
    assert_eq!(interrupted.run_status, "interrupted");
    assert_eq!(interrupted.event_status, "interrupted");
    assert_eq!(interrupted.steps, 1);
    assert_eq!(interrupted.finish_reason, "interrupted");
    assert_eq!(interrupted.error.as_deref(), Some("cancelled"));
}

#[test]
fn session_runner_facade_builds_shared_provider_step_outcomes() {
    let complete = SessionRunnerFacade::provider_step_outcome(0, "stop");
    assert!(complete.is_complete());
    assert!(!complete.continues_with_tools());
    assert_eq!(complete.tool_call_count, 0);
    assert_eq!(complete.finish_reason, "stop");

    let continue_with_tools = SessionRunnerFacade::provider_step_outcome(2, "tool_call");
    assert!(!continue_with_tools.is_complete());
    assert!(continue_with_tools.continues_with_tools());
    assert_eq!(continue_with_tools.tool_call_count, 2);
    assert_eq!(continue_with_tools.finish_reason, "tool_call");
}

#[test]
fn session_runner_facade_builds_shared_turn_usage_and_trace_payloads() {
    let facade = SessionRunnerFacade::new("/tmp/openagent-session-runner", "session_trace");

    let usage = SessionRunnerFacade::estimated_turn_usage_payload("hello world", "done", 2);
    assert_eq!(usage["input_tokens"], 3);
    assert_eq!(usage["output_tokens"], 1);
    assert_eq!(usage["tool_tokens"], 32);
    assert_eq!(usage["total_tokens"], 36);
    assert_eq!(usage["tool_calls"], 2);
    assert_eq!(usage["estimated"], true);

    let trace =
        facade.turn_trace_payload("run_trace", "reviewer", "gpt-test", "default", "medium", 2);
    assert_eq!(trace["run_id"], "run_trace");
    assert_eq!(trace["session_id"], "session_trace");
    assert_eq!(trace["agent"], "reviewer");
    assert_eq!(trace["model"], "gpt-test");
    assert_eq!(trace["variant"], "default");
    assert_eq!(trace["thinking"], "medium");
    assert_eq!(trace["tool_calls"], 2);

    let attrs = SessionRunnerFacade::model_usage_event_attributes(
        &Usage {
            input_tokens: 11,
            output_tokens: 7,
            cost: 0.25,
        },
        "openai:chat",
        3,
    );
    assert_eq!(attrs["input_tokens"], 11);
    assert_eq!(attrs["output_tokens"], 7);
    assert_eq!(attrs["cost"], 0.25);
    assert_eq!(attrs["source"], "openai:chat");
    assert_eq!(attrs["tool_calls"], 3);
}

#[test]
fn file_tools_enforce_path_safety_read_before_write_and_metadata() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-file")?;
    fs::write(root.join("notes.txt"), "alpha\nbeta\ngamma\n")?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src").join("main.rs"),
        "fn main() {\n  println!(\"beta\");\n}\n",
    )?;
    for runtime_dir in ["target", "jobs/run-1", ".openagent/sessions", "dist"] {
        fs::create_dir_all(root.join(runtime_dir))?;
    }
    fs::write(
        root.join("target").join("generated.rs"),
        "fn generated() {\n  println!(\"delta\");\n}\n",
    )?;
    fs::write(
        root.join("jobs").join("run-1").join("job.rs"),
        "fn job() {\n  println!(\"delta\");\n}\n",
    )?;
    fs::write(
        root.join(".openagent").join("sessions").join("trace.rs"),
        "delta\n",
    )?;
    fs::write(root.join("dist").join("bundle.rs"), "delta\n")?;

    let toolkit = Toolkit::with_builtins();
    let mut ctx = ToolContext::new(&root).with_session_id("session/file");

    let escaped = toolkit.execute(
        "read",
        json!({"file_path": "../outside.txt"}),
        "call_escape",
        &mut ctx,
    );
    assert!(
        escaped
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Path escapes session root")
    );

    let blocked_write = toolkit.execute(
        "write",
        json!({"file_path": "notes.txt", "content": "blocked"}),
        "call_blocked_write",
        &mut ctx,
    );
    assert!(
        blocked_write
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Must read existing file before writing")
    );

    let read = toolkit.execute(
        "read",
        json!({"file_path": "notes.txt", "offset": 1, "limit": 1}),
        "call_read",
        &mut ctx,
    );
    assert!(read.error.is_none());
    assert_eq!(
        read.output,
        "<file>\n00002| beta\n\n(File has more lines. Use 'offset' parameter to read beyond line 2)\n</file>"
    );
    assert_eq!(read.metadata["preview"], json!("beta"));
    assert_eq!(read.metadata["tool"], json!("read"));
    assert_eq!(read.metadata["title"], json!("notes.txt"));
    assert_eq!(read.metadata["truncated"], json!(true));

    let edit = toolkit.execute(
        "edit",
        json!({
            "file_path": "notes.txt",
            "old_string": "beta",
            "new_string": "delta",
        }),
        "call_edit",
        &mut ctx,
    );
    assert!(edit.error.is_none());
    assert_eq!(
        fs::read_to_string(root.join("notes.txt"))?,
        "alpha\ndelta\ngamma\n"
    );
    assert_eq!(edit.metadata["replace_all"], json!(false));

    let write_new = toolkit.execute(
        "write",
        json!({"file_path": "new.txt", "content": "fresh"}),
        "call_write_new",
        &mut ctx,
    );
    assert!(write_new.error.is_none());
    assert_eq!(write_new.metadata["exists"], json!(false));

    let glob = toolkit.execute("glob", json!({"pattern": "**/*.rs"}), "call_glob", &mut ctx);
    assert!(glob.output.contains("main.rs"));
    assert!(!glob.output.contains("generated.rs"));
    assert!(!glob.output.contains("job.rs"));
    assert!(!glob.output.contains("trace.rs"));
    assert!(!glob.output.contains("bundle.rs"));
    assert_eq!(glob.metadata["count"], json!(1));

    let grep = toolkit.execute(
        "grep",
        json!({"pattern": "println", "include": "*.rs"}),
        "call_grep",
        &mut ctx,
    );
    assert!(grep.output.contains("Found 1 matches"));
    assert!(!grep.output.contains("generated.rs"));
    assert!(!grep.output.contains("job.rs"));
    assert_eq!(grep.metadata["include"], json!("*.rs"));

    let ls = toolkit.execute("ls", json!({"ignore": ["new.txt"]}), "call_ls", &mut ctx);
    assert!(ls.output.contains("notes.txt"));
    assert!(!ls.output.contains("new.txt"));
    assert!(!ls.output.contains("target"));
    assert!(!ls.output.contains("jobs"));
    assert!(!ls.output.contains(".openagent"));
    assert!(!ls.output.contains("dist"));
    assert!(ls.metadata["count"].as_u64().unwrap_or_default() >= 2);

    let code_search = toolkit.execute(
        "code_search",
        json!({"query": "delta", "glob": "*.txt"}),
        "call_code_search",
        &mut ctx,
    );
    assert!(code_search.output.contains("notes.txt"));
    assert!(!code_search.output.contains("generated.rs"));
    assert!(!code_search.output.contains("job.rs"));
    assert!(!code_search.output.contains("trace.rs"));
    assert!(!code_search.output.contains("bundle.rs"));
    assert_eq!(code_search.metadata["count"], json!(1));

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn lsp_tool_reports_status_and_queries_configured_server() -> Result<(), Box<dyn Error>> {
    if !command_available("python3") {
        return Ok(());
    }
    let root = unique_temp_dir("openagent-tools-lsp")?;
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fake\"\n")?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    let fake = write_fake_lsp_server(&root)?;
    fs::create_dir_all(root.join(".openagent"))?;
    fs::write(
        root.join(".openagent/lsp.json"),
        serde_json::to_string_pretty(&json!({
            "servers": {
                "fake": {
                    "command": ["python3", fake],
                    "extensions": [".rs"],
                    "root_markers": ["Cargo.toml"]
                }
            }
        }))?,
    )?;

    let toolkit = Toolkit::with_builtins();
    let mut ctx = ToolContext::new(&root).with_session_id("session-lsp");
    let status = toolkit.execute(
        "lsp",
        json!({"operation": "status"}),
        "call_lsp_status",
        &mut ctx,
    );
    assert!(status.error.is_none());
    assert_eq!(
        status.metadata["server_count"].as_u64().unwrap_or_default() >= 1,
        true
    );
    assert!(status.output.contains("\"fake\""));

    let symbols = toolkit.execute(
        "lsp",
        json!({"operation": "documentSymbol", "file_path": "src/main.rs", "timeout_ms": 3000}),
        "call_lsp_symbols",
        &mut ctx,
    );
    assert!(symbols.error.is_none(), "{symbols:?}");
    assert_eq!(symbols.metadata["server_id"], json!("fake"));
    assert!(symbols.output.contains("\"name\": \"main\""));

    let escaped = toolkit.execute(
        "lsp",
        json!({"operation": "documentSymbol", "file_path": "../main.rs"}),
        "call_lsp_escape",
        &mut ctx,
    );
    assert!(
        escaped
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Path escapes session root")
    );

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn shell_runtime_blocks_destructive_commands_and_saves_truncated_output()
-> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-shell")?;
    let mut toolkit = Toolkit::with_builtins();
    toolkit.max_output_bytes = 8;
    let mut ctx = ToolContext::new(&root).with_session_id("session-shell");

    let blocked = toolkit.execute(
        "bash",
        json!({"command": "printf ok; rm -rf tmp"}),
        "call_rm",
        &mut ctx,
    );
    assert!(
        blocked
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("rm command is disabled")
    );
    assert_eq!(
        blocked_command("printf ok; rm -rf tmp"),
        Some("rm".to_string())
    );
    assert!(!benchmark_mode_allows_shell_command(&ctx));
    assert!(benchmark_mode_value_allows_shell_command("terminal-bench"));
    assert!(benchmark_mode_value_allows_shell_command("terminal_bench"));
    assert!(!benchmark_mode_value_allows_shell_command("local"));

    let mut benchmark_ctx = ToolContext::new(&root).with_session_id("session-shell-benchmark");
    benchmark_ctx
        .execution_metadata
        .insert("benchmark_mode".to_string(), json!("terminal-bench"));
    assert!(benchmark_mode_allows_shell_command(&benchmark_ctx));
    let benchmark_allowed = toolkit.execute(
        "bash",
        json!({"command": "printf ok; rm -rf openagent-benchmark-missing"}),
        "call_rm_benchmark",
        &mut benchmark_ctx,
    );
    assert!(benchmark_allowed.error.is_none());
    assert_eq!(benchmark_allowed.output, "ok");

    let result = toolkit.execute(
        "bash",
        json!({"command": "printf abcdefghijklmnopqrstuvwxyz", "description": "long output"}),
        "call_long",
        &mut ctx,
    );
    assert!(result.error.is_none());
    assert!(result.output.contains("... output truncated"));
    assert_eq!(result.metadata["output_truncated"], json!(true));
    let output_path = result.metadata["output_path"].as_str().unwrap_or_default();
    assert_eq!(
        fs::read_to_string(output_path)?,
        "abcdefghijklmnopqrstuvwxyz"
    );

    let runtime = LocalWorkspaceRuntime::new(&root);
    let command = runtime.run_command("printf runtime-ok", None, 120_000)?;
    assert_eq!(command.returncode, 0);
    assert_eq!(command.stdout, "runtime-ok");
    assert!(runtime.resolve_path(Some("../outside"), true).is_err());

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn toolkit_enforces_permission_rules_before_execution() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-permission")?;
    fs::write(root.join("notes.txt"), "alpha\n")?;
    let toolkit = Toolkit::with_builtins();

    let mut readonly = ToolContext::new(&root)
        .with_session_id("session/readonly")
        .with_permission_ruleset(PermissionRuleset::Readonly);
    let read = toolkit.execute(
        "read",
        json!({"file_path": "notes.txt"}),
        "call_read_allowed",
        &mut readonly,
    );
    assert!(read.error.is_none());
    assert_eq!(read.metadata["tool"], json!("read"));

    let denied_write = toolkit.execute(
        "write",
        json!({"file_path": "denied.txt", "content": "nope"}),
        "call_write_denied",
        &mut readonly,
    );
    assert!(
        denied_write
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Permission denied")
    );
    assert_eq!(denied_write.metadata["permission_action"], json!("deny"));
    assert_eq!(
        denied_write.metadata["error_kind"],
        json!("permission_denied")
    );
    assert!(!root.join("denied.txt").exists());

    let mut plan_only = ToolContext::new(&root)
        .with_session_id("session/plan")
        .with_permission_ruleset(PermissionRuleset::PlanOnly);
    let needs_approval = toolkit.execute(
        "bash",
        json!({"command": "printf blocked"}),
        "call_bash_ask",
        &mut plan_only,
    );
    assert!(
        needs_approval
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Permission requires user confirmation")
    );
    assert_eq!(needs_approval.metadata["permission_action"], json!("ask"));
    assert_eq!(needs_approval.metadata["requires_approval"], json!(true));
    assert_eq!(
        needs_approval.metadata["input"]["command"],
        json!("printf blocked")
    );

    let mut auto_allow = ToolContext::new(&root)
        .with_session_id("session/auto")
        .with_permission_ruleset(PermissionRuleset::PlanOnly)
        .with_dangerously_skip_permissions(true);
    let allowed_by_skip = toolkit.execute(
        "bash",
        json!({"command": "printf allowed"}),
        "call_bash_auto_allow",
        &mut auto_allow,
    );
    assert!(allowed_by_skip.error.is_none());
    assert_eq!(allowed_by_skip.output, "allowed");

    let mut auto_deny = ToolContext::new(&root)
        .with_session_id("session/auto-deny")
        .with_permission_ruleset(PermissionRuleset::Readonly)
        .with_dangerously_skip_permissions(true);
    let still_denied = toolkit.execute(
        "write",
        json!({"file_path": "still-denied.txt", "content": "nope"}),
        "call_write_still_denied",
        &mut auto_deny,
    );
    assert_eq!(still_denied.metadata["permission_action"], json!("deny"));
    assert!(!root.join("still-denied.txt").exists());

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn web_fetch_reads_http_sources_under_readonly_permissions() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-web-fetch")?;
    let (port, server) = spawn_static_http_server(
        "OpenAgent Scout docs\nEvidence: dependency version guidance.\n",
        "text/plain; charset=utf-8",
    )?;
    let toolkit = Toolkit::with_builtins();
    let mut ctx = ToolContext::new(&root)
        .with_session_id("session/web-fetch")
        .with_permission_ruleset(PermissionRuleset::Readonly);
    let fetched = toolkit.execute(
        "web_fetch",
        json!({"url": format!("http://127.0.0.1:{port}/docs"), "max_bytes": 4096}),
        "call_web_fetch",
        &mut ctx,
    );
    assert!(fetched.error.is_none(), "{fetched:?}");
    assert!(fetched.output.contains("OpenAgent Scout docs"));
    assert_eq!(fetched.metadata["tool"], json!("web_fetch"));
    assert_eq!(fetched.metadata["status"], json!(200));
    assert_eq!(
        fetched.metadata["content_type"],
        json!("text/plain; charset=utf-8")
    );
    assert_eq!(fetched.metadata["truncated"], json!(false));

    let rejected_scheme = toolkit.execute(
        "web_fetch",
        json!({"url": "file:///etc/passwd"}),
        "call_web_fetch_file",
        &mut ctx,
    );
    assert!(
        rejected_scheme
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("only supports http and https")
    );

    server
        .join()
        .map_err(|_| "web fetch fixture server panicked")?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn task_subagent_router_selects_unique_description_match() {
    let subagents = vec![
        TaskSubagentDescriptor {
            id: "explore".to_string(),
            name: "Explore".to_string(),
            description: "Read-only code exploration subagent for fast search and mapping."
                .to_string(),
        },
        TaskSubagentDescriptor {
            id: "scout".to_string(),
            name: "Scout".to_string(),
            description: "External documentation and dependency research subagent with web access."
                .to_string(),
        },
        TaskSubagentDescriptor {
            id: "general".to_string(),
            name: "General".to_string(),
            description: "General-purpose subagent for complex multi-step work.".to_string(),
        },
    ];

    let routed = select_task_subagent_for_prompt(
        &subagents,
        "Research the external dependency docs before changing this integration.",
    )
    .expect("expected scout route");
    assert_eq!(routed.subagent_id, "scout");
    assert!(routed.score >= 4);
    assert!(routed.matched_terms.contains(&"dependency".to_string()));
    assert!(routed.matched_terms.contains(&"documentation".to_string()));

    assert!(select_task_subagent_for_prompt(&subagents, "Please handle this task.").is_none());
}

#[test]
fn prepare_isolated_workspace_copies_workspace_without_heavy_dirs() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-isolation")?;
    let source = root.join("workspace");
    let isolation_root = root.join("isolated");
    fs::create_dir_all(source.join("src"))?;
    fs::create_dir_all(source.join("target"))?;
    fs::create_dir_all(source.join("jobs"))?;
    fs::create_dir_all(source.join(".openagent"))?;
    fs::create_dir_all(source.join("dist"))?;
    fs::write(source.join("src").join("main.rs"), "fn main() {}\n")?;
    fs::write(source.join("target").join("cache.txt"), "heavy\n")?;
    fs::write(source.join("jobs").join("job.log"), "heavy\n")?;
    fs::write(source.join(".openagent").join("session.json"), "heavy\n")?;
    fs::write(source.join("dist").join("bundle.js"), "heavy\n")?;

    let isolation = prepare_isolated_workspace(&source, &isolation_root, "task/one")?;
    let isolated = PathBuf::from(&isolation.workspace);
    assert_eq!(isolation.enabled, true);
    assert_eq!(isolation.method, "directory_copy");
    assert!(isolated.join("src").join("main.rs").exists());
    assert!(!isolated.join("target").exists());
    assert!(!isolated.join("jobs").exists());
    assert!(!isolated.join(".openagent").exists());
    assert!(!isolated.join("dist").exists());
    fs::write(isolated.join("child.txt"), "isolated\n")?;
    assert!(!source.join("child.txt").exists());

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn todo_memory_and_question_tools_round_trip_session_state() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-state")?;
    let toolkit = Toolkit::with_builtins();
    let mut ctx = ToolContext::new(&root).with_session_id("session/state");

    let missing = toolkit.execute(
        "memory_read",
        json!({"key": "missing"}),
        "call_mem_missing",
        &mut ctx,
    );
    assert_eq!(missing.output, "null");
    let write = toolkit.execute(
        "memory_write",
        json!({"key": "profile", "value": {"name": "Ada"}}),
        "call_mem_write",
        &mut ctx,
    );
    assert_eq!(write.output, "ok");
    let read = toolkit.execute(
        "memory_read",
        json!({"key": "profile"}),
        "call_mem_read",
        &mut ctx,
    );
    assert_eq!(read.output, "{\"name\":\"Ada\"}");

    let todos = vec![TodoItem::new(
        "port tools",
        "in_progress",
        "high",
        "todo-fixture",
    )];
    let todo_write = toolkit.execute(
        "todowrite",
        json!({"todos": todos}),
        "call_todo_write",
        &mut ctx,
    );
    assert_eq!(todo_write.metadata["title"], json!("1 todos"));
    assert!(todo_write.output.contains("\"id\": \"todo-fixture\""));
    let todo_read = toolkit.execute("todoread", json!({}), "call_todo_read", &mut ctx);
    assert_eq!(todo_read.output, todo_write.output);

    ctx.set_question_answers(vec![vec!["Fast".to_string()]]);
    let question = toolkit.execute(
        "question",
        json!({
            "questions": [{
                "header": "Mode",
                "question": "Pick a mode",
                "options": [{"label": "Fast", "description": "Run quickly"}],
            }]
        }),
        "call_question",
        &mut ctx,
    );
    assert_eq!(
        question.output,
        "User has answered your questions: \"Pick a mode\"=\"Fast\". You can now continue with the user's answers in mind."
    );
    assert_eq!(question.metadata["count"], json!(1));

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn skill_tool_lists_loads_filters_and_respects_explicit_roots() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-skill")?;
    let workspace = root.join("workspace");
    let shared = root.join("shared-skills");
    fs::create_dir_all(&workspace)?;
    write_skill(
        &workspace,
        ".openagent/skills/code-review/SKILL.md",
        "code-review",
        "Review code carefully",
        "Inspect diffs and tests.",
    )?;
    let code_review_resource = workspace.join(".openagent/skills/code-review/references");
    fs::create_dir_all(&code_review_resource)?;
    fs::write(
        code_review_resource.join("checklist.md"),
        "Review checklist resource.",
    )?;
    fs::write(root.join("outside.txt"), "Outside skill directory.")?;
    write_skill(
        &workspace,
        ".openagent/skills/research/SKILL.md",
        "research",
        "Research external sources",
        "Collect evidence.",
    )?;
    write_skill(
        &shared,
        "review/SKILL.md",
        "review",
        "Shared review",
        "Use shared guidance.",
    )?;

    let toolkit = Toolkit::with_builtins();
    let mut ctx = ToolContext::new(&workspace).with_session_id("session-skill");

    let listed = toolkit.execute(
        "skill",
        json!({"query": "review", "include_content": true}),
        "call_skill_list",
        &mut ctx,
    );
    assert!(listed.error.is_none());
    assert!(listed.output.contains("Matched skills for \"review\""));
    assert!(listed.output.contains("code-review"));
    assert!(listed.output.contains("Inspect diffs and tests."));
    assert_eq!(listed.metadata["query"], json!("review"));

    let loaded = toolkit.execute(
        "skill",
        json!({"name": "code-review"}),
        "call_skill_load",
        &mut ctx,
    );
    assert!(loaded.error.is_none());
    assert!(
        loaded
            .output
            .contains("<skill_content name=\"code-review\">")
    );
    assert!(loaded.output.contains("<base_directory>"));
    assert!(loaded.output.contains("<skill_files sampled=\"1\""));
    assert!(
        loaded
            .output
            .contains("<file>references/checklist.md</file>")
    );
    assert!(!loaded.output.contains("outside.txt"));
    assert!(loaded.output.contains("Inspect diffs and tests."));
    assert_eq!(loaded.metadata["skill_name"], json!("code-review"));
    assert_eq!(
        loaded.metadata["skill_files"],
        json!(["references/checklist.md"])
    );

    let missing = toolkit.execute(
        "skill",
        json!({"name": "missing"}),
        "call_skill_missing",
        &mut ctx,
    );
    assert!(
        missing
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Skill \"missing\" not found")
    );

    ctx.agent_options.insert(
        "skill_roots".to_string(),
        json!([shared.to_string_lossy().to_string()]),
    );
    let explicit = toolkit.execute("skill", json!({}), "call_skill_explicit", &mut ctx);
    assert!(explicit.output.contains("review"));
    assert!(!explicit.output.contains("code-review"));

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn skill_tool_supports_claude_frontmatter_subset() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-skill-frontmatter")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join("src"))?;
    fs::write(workspace.join("src/main.rs"), "fn main() {}\n")?;
    fs::create_dir_all(workspace.join(".openagent/skills/claude"))?;
    fs::write(
        workspace.join(".openagent/skills/claude/SKILL.md"),
        r#"---
name: claude
description: Claude-style skill
when_to_use: Use when editing Rust entrypoints.
paths:
  - src/*.rs
allowed-tools:
  - read
disallowed-tools:
  - write
arguments:
  - topic
---
Work on {{topic}} with $ARGUMENTS.
"#,
    )?;
    fs::create_dir_all(workspace.join(".openagent/skills/hidden"))?;
    fs::write(
        workspace.join(".openagent/skills/hidden/SKILL.md"),
        r#"---
name: hidden
description: Hidden skill
user-invocable: false
---
Hidden guidance.
"#,
    )?;
    fs::create_dir_all(workspace.join(".openagent/skills/disabled"))?;
    fs::write(
        workspace.join(".openagent/skills/disabled/SKILL.md"),
        r#"---
name: disabled
description: Disabled skill
disable-model-invocation: true
---
Disabled guidance.
"#,
    )?;

    let toolkit = Toolkit::with_builtins();
    let mut ctx = ToolContext::new(&workspace).with_session_id("session-skill-frontmatter");
    let listed = toolkit.execute(
        "skill",
        json!({"path": "src/main.rs"}),
        "call_skill_list_frontmatter",
        &mut ctx,
    );
    assert!(listed.error.is_none());
    assert!(listed.output.contains("claude"));
    assert!(
        listed
            .output
            .contains("When to use: Use when editing Rust entrypoints.")
    );
    assert!(!listed.output.contains("hidden"));
    assert!(!listed.output.contains("disabled"));

    let mismatched_list = toolkit.execute(
        "skill",
        json!({"path": "README.md"}),
        "call_skill_list_mismatch",
        &mut ctx,
    );
    assert!(!mismatched_list.output.contains("claude"));
    let mismatched_load = toolkit.execute(
        "skill",
        json!({"name": "claude", "path": "README.md"}),
        "call_skill_load_mismatch",
        &mut ctx,
    );
    assert!(
        mismatched_load
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("does not match requested path scope")
    );

    let hidden_load = toolkit.execute(
        "skill",
        json!({"name": "hidden"}),
        "call_hidden_skill",
        &mut ctx,
    );
    assert!(
        hidden_load
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("disabled for model invocation")
    );
    let disabled_load = toolkit.execute(
        "skill",
        json!({"name": "disabled"}),
        "call_disabled_skill",
        &mut ctx,
    );
    assert!(
        disabled_load
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("disabled for model invocation")
    );

    let loaded = toolkit.execute(
        "skill",
        json!({
            "name": "claude",
            "path": "src/main.rs",
            "arguments": {"topic": "routing"}
        }),
        "call_skill_load_frontmatter",
        &mut ctx,
    );
    assert!(loaded.error.is_none());
    assert!(loaded.output.contains("Work on routing"));
    assert!(loaded.output.contains(r#"{"topic":"routing"}"#));
    assert_eq!(
        loaded.metadata["skill_arguments"],
        json!({"topic": "routing"})
    );

    let read = toolkit.execute(
        "read",
        json!({"file_path": "src/main.rs"}),
        "call_read_allowed_by_skill",
        &mut ctx,
    );
    assert!(read.error.is_none());
    let write = toolkit.execute(
        "write",
        json!({"file_path": "blocked.txt", "content": "nope"}),
        "call_write_blocked_by_skill",
        &mut ctx,
    );
    assert!(
        write
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("disallowed by loaded skill")
    );
    assert_eq!(write.metadata["error_kind"], json!("skill_tool_restricted"));
    assert!(!workspace.join("blocked.txt").exists());

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn skill_tool_lists_builtin_skills_and_workspace_overrides() -> Result<(), Box<dyn Error>> {
    let root = unique_temp_dir("openagent-tools-builtin-skill")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;

    let toolkit = Toolkit::with_builtins();
    let mut ctx = ToolContext::new(&workspace).with_session_id("session-builtin-skill");
    let listed = toolkit.execute(
        "skill",
        json!({"query": "openai-docs", "limit": 5}),
        "call_skill_builtin",
        &mut ctx,
    );
    assert!(listed.error.is_none());
    assert!(listed.output.contains("openai-docs"));

    write_skill(
        &workspace,
        ".openagent/skills/openai-docs/SKILL.md",
        "openai-docs",
        "Workspace OpenAI docs override",
        "Workspace override content.",
    )?;
    let loaded = toolkit.execute(
        "skill",
        json!({"name": "openai-docs"}),
        "call_skill_override",
        &mut ctx,
    );
    assert!(loaded.error.is_none());
    assert!(loaded.output.contains("Workspace override content."));
    assert!(
        loaded.metadata["skill_location"]
            .as_str()
            .is_some_and(|location| location.contains(".openagent/skills/openai-docs"))
    );

    let empty = root.join("empty");
    fs::create_dir_all(&empty)?;
    let mut disabled = ToolContext::new(&empty).with_session_id("session-builtin-disabled");
    disabled
        .agent_options
        .insert("include_builtin_skills".to_string(), json!(false));
    let hidden = toolkit.execute(
        "skill",
        json!({"query": "openai-docs"}),
        "call_skill_builtin_disabled",
        &mut disabled,
    );
    assert!(hidden.output.contains("No skills matched query"));

    fs::remove_dir_all(root)?;
    Ok(())
}

fn tool_runtime_fixture() -> Result<Value, Box<dyn Error>> {
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);
    let selected = [
        "read",
        "write",
        "edit",
        "glob",
        "grep",
        "ls",
        "bash",
        "code_search",
        "lsp",
        "memory_read",
        "memory_write",
        "todowrite",
        "todoread",
        "question",
    ];
    let mut tools = serde_json::Map::new();
    for tool_id in selected {
        let tool = registry
            .get(tool_id)
            .ok_or_else(|| format!("missing tool: {tool_id}"))?;
        let properties = sorted_property_names(&tool.parameter_schema);
        let required = tool
            .parameter_schema
            .get("required")
            .cloned()
            .unwrap_or_else(|| json!([]));
        tools.insert(
            tool_id.to_string(),
            json!({
                "group": tool.group,
                "dangerous": tool.dangerous,
                "execution_scope": to_value(&tool.execution_scope)?,
                "execution_schema": to_value(&tool.execution_schema)?,
                "parameter_schema": {
                    "required": required,
                    "properties": properties,
                },
            }),
        );
    }

    let read_format = format_read_output_from_text("alpha\nbeta\ngamma\n", 1, 1);
    let path_escape_error = ensure_within_root("/tmp/openagent-fixture", "/tmp/outside.txt")
        .err()
        .ok_or_else(|| "path escape fixture did not fail".to_string())?;
    let todo_output = serde_json::to_string_pretty(&vec![TodoItem::new(
        "port tools",
        "in_progress",
        "high",
        "todo-fixture",
    )])?;

    Ok(json!({
        "schema_version": 1,
        "tools": Value::Object(tools),
        "registry_namespace": {
            "default": qualify_tool_id("fixture", "default"),
            "custom": qualify_tool_id("fixture", "custom"),
        },
        "execution_schemas": {
            "readonly": to_value(readonly_schema("workspace-read", false, true, None))?,
            "exclusive": to_value(exclusive_schema(
                "workspace-write",
                true,
                true,
                false,
                false,
                false,
                Some("file:{file_path}"),
            ))?,
        },
        "read_format": to_value(read_format)?,
        "truncation": {
            "line": to_value(truncate_output("L1\nL2\nL3", Some(2), Some(999)))?,
            "byte": to_value(truncate_output("abcdef", Some(999), Some(4)))?,
        },
        "path_escape_error": path_escape_error,
        "blocked_shell_command": blocked_command("printf ok; rm -rf tmp"),
        "todo_output": todo_output,
        "memory_outputs": {"missing": "null", "write": "ok"},
        "question_output": "User has answered your questions: \"Pick a mode\"=\"Fast\". You can now continue with the user's answers in mind.",
    }))
}

fn sorted_property_names(schema: &Value) -> Vec<String> {
    let mut properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|items| items.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    properties.sort();
    properties
}

fn to_value<T: Serialize>(value: T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value)
}

fn spawn_static_http_server(
    body: &str,
    content_type: &str,
) -> Result<(u16, thread::JoinHandle<()>), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let body = body.to_string();
    let content_type = content_type.to_string();
    Ok((
        port,
        thread::spawn(move || {
            let Ok((mut stream, _addr)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }),
    ))
}

fn unique_temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    };
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn write_skill(
    base: &Path,
    relative: &str,
    name: &str,
    description: &str,
    body: &str,
) -> Result<(), Box<dyn Error>> {
    let path = base.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )?;
    Ok(())
}

fn write_fake_lsp_server(root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let path = root.join("fake_lsp.py");
    fs::write(
        &path,
        r#"
import json
import sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode("utf-8").split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        return None
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))

def send(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

def result(id, value):
    send({"jsonrpc": "2.0", "id": id, "result": value})

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    id = message.get("id")
    params = message.get("params") or {}
    uri = ((params.get("textDocument") or {}).get("uri")) or "file:///fake.rs"
    if method == "initialize":
        result(id, {"capabilities": {"textDocumentSync": {"change": 1}}})
    elif method in ("initialized", "workspace/didChangeConfiguration"):
        pass
    elif method == "shutdown":
        result(id, None)
    elif method == "exit":
        break
    elif method == "textDocument/documentSymbol":
        result(id, [{
            "name": "main",
            "kind": 12,
            "location": {
                "uri": uri,
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 9}}
            }
        }])
    elif id is not None:
        result(id, [])
"#,
    )?;
    Ok(path)
}

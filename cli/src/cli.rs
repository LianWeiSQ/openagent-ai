//! CLI crate for the Rust rewrite.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use openagent_core::{
    AgentSystemPromptInput, PermissionManager, SkillDocument, SkillRegistry, SkillRegistryOptions,
    build_agent_system_prompt, permission_rule, skill_document_model_invocable,
};
use openagent_mcp::{
    McpTransport, RemoteMcpManager, RemoteMcpToolDescriptor, bridge_tool_output,
    build_tool_descriptors_from_values, load_mcp_config, mcp_tool_definition,
    normalize_tool_call_result, sanitize_mcp_observation_value, unavailable_tool_result,
};
use openagent_protocol::ToolResult;
use openagent_protocol::{ChatMessage, PermissionRuleset, Role, ToolCall, ToolSchema, Usage};
use openagent_provider::{
    AnthropicLanguageModelConfig, OpenAiLanguageModelConfig, ProviderStreamEvent, anthropic_model,
    build_anthropic_payload, build_openai_chat_payload, build_openai_responses_payload,
    default_env_mapping, normalize_anthropic_events, normalize_openai_chat_sse_chunks,
    normalize_openai_responses_response, normalize_provider, openai_compatible_model,
    parse_tool_arguments, provider_auth_methods, provider_default_base_url, provider_default_model,
    provider_label, provider_requires_api_key, summarize_http_error_body,
};
use openagent_session::{
    FileSessionStore, Session, SessionEventOptions, SessionForkBoundary, SessionPartOptions,
    SessionStatus, StartRunOptions,
};
use openagent_tools::{
    SkillPermissionRule, TASK_TOOL_ID, TaskPermissionRule, TaskSubagentDescriptor, ToolContext,
    Toolkit, fork_skill_task_from_input, parse_agent_profile_schema, register_task_tool,
    skill_is_visible, task_subagent_is_visible,
};
use serde_json::{Map, Value, json};

mod agents;
mod auth;
mod client;
mod commands;
mod config;
mod diagnostics;
mod doctor;
mod fixtures;
mod github;
mod help;
mod interactions;
mod lsp;
mod mcp;
mod models;
mod prompt;
mod remote;
mod sessions;
mod skills;
mod util;

use agents::{agent_command, agent_registry_dir, plugin_command};
use auth::auth_command;
use client::client_command;
use commands::{custom_command, discover_custom_commands, render_custom_template};
use config::config_command;
use diagnostics::{
    acp_command, console_command, db_command, debug_command, generate_command, lifecycle_command,
    stats_command,
};
use doctor::{doctor_payload_from_args, doctor_text_from_payload};
use github::{github_command, pr_command};
use help::*;
use interactions::{approval_command, question_command};
use lsp::lsp_command;
use mcp::mcp_command;
use models::{models_cache_path, models_command};
use prompt::{run_prompt_command, run_prompt_command_with_events, split_answer_items};
use remote::{
    attach_command, http_runtime_command, remote_auth_from_args, remote_events_for_payload,
    remote_select_session, remote_select_session_with_auth, remote_start_turn,
    remote_start_turn_with_auth, terminal_command, text_from_bridge_events, tui_command,
};
use sessions::{
    latest_session_id, session_command, session_export, session_import, session_list, share_session,
};
use skills::skills_command;

use util::*;

pub use fixtures::{
    auth_list_payload, auth_login_payload, auth_methods_payload, cli_commands_fixture,
    config_init_payload, config_show_payload, core_crate_name, custom_command_list_payload,
    custom_command_render_json_payload, custom_command_render_text_result,
    custom_command_show_payload, doctor_anthropic_payload, doctor_json_failed_payload,
    doctor_json_failed_result, doctor_text_ok_result, mcp_add_payload, mcp_doctor_payload,
    mcp_list_table_result, model_env_fixture, parse_cli_args, rendered_custom_command_prompt,
};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const DEFAULT_BASE_URL: &str = "http://localhost:8080";
pub const DEFAULT_MODEL: &str = "gpt-5.5";
pub const DEFAULT_WIRE_API: &str = "responses";
pub const DEFAULT_MAX_STEPS: &str = "0";
pub const UNBOUNDED_MAX_STEPS: u64 = u64::MAX / 4;
pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8787";
pub const DEFAULT_SERVER_TOKEN_ENV: &str = "OPENAGENT_SERVER_TOKEN";
pub const GOAL10_ROOT: &str = "/tmp/openagent-rust-rewrite-fixture-goal10";
pub const GOAL10_WORKSPACE: &str = "/tmp/openagent-rust-rewrite-fixture-goal10/workspace";
const RUN_POSITIONAL_VALUE_FLAGS: &[&str] = &[
    "--workspace",
    "--dir",
    "--config",
    "--env-file",
    "--auth-file",
    "--base-url",
    "--session-root",
    "--session",
    "-s",
    "--file",
    "-f",
    "--command",
    "--command-dir",
    "--format",
    "--model",
    "-m",
    "--provider",
    "--wire-api",
    "--api-key",
    "--mcp-config",
    "--max-steps",
    "--max-output-tokens",
    "--timeout-s",
    "--timeout-ms",
    "--agent",
    "--title",
    "--attach",
    "--server-token",
    "--server-token-env",
    "--answer",
    "--permission",
    "--password",
    "-p",
    "--username",
    "-u",
    "--variant",
    "--port",
    "--column",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliRunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CliRunResult {
    #[must_use]
    pub fn ok_json(value: &Value) -> Self {
        Self {
            exit_code: 0,
            stdout: format!("{}\n", stable_json_dumps(value)),
            stderr: String::new(),
        }
    }
}

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub const fn command_name() -> &'static str {
    "openagent"
}

#[must_use]
pub fn run_cli_command(argv: &[String]) -> CliRunResult {
    if argv.is_empty() {
        return CliRunResult {
            exit_code: 0,
            stdout: format!("{}\n", command_name()),
            stderr: String::new(),
        };
    }
    if is_help_flag(&argv[0]) || argv[0] == "help" {
        return ok_text(root_help());
    }
    match argv[0].as_str() {
        "tui" => tui_command(&argv[1..]),
        "serve" => http_runtime_command(&argv[1..], serve_help()),
        "attach" => attach_command(&argv[1..]),
        "terminal" => terminal_command(&argv[1..]),
        "run" => run_prompt_command(&argv[1..]),
        "client" => client_command(&argv[1..]),
        "session" => session_command(&argv[1..]),
        "models" => models_command(&argv[1..]),
        "stats" => stats_command(&argv[1..]),
        "command" => custom_command(&argv[1..]),
        "skill" | "skills" => skills_command(&argv[1..]),
        "lsp" => lsp_command(&argv[1..]),
        "config" => config_command(&argv[1..]),
        "auth" | "providers" => auth_command(argv[0].as_str(), &argv[1..]),
        "mcp" => mcp_command(&argv[1..]),
        "approval" => approval_command(&argv[1..]),
        "question" => question_command(&argv[1..]),
        "doctor" => {
            if argv.iter().any(|arg| is_help_flag(arg)) {
                return ok_text(doctor_help());
            }
            let format = argv
                .windows(2)
                .find_map(|items| (items[0] == "--format").then_some(items[1].as_str()))
                .unwrap_or("text");
            let provider = env::var("OPENAGENT_PROVIDER")
                .or_else(|_| env::var("OPENAGENT_ACTIVE_PROVIDER"))
                .unwrap_or_else(|_| "openai".to_string());
            let payload = doctor_payload_from_args(&provider, &argv[1..]);
            let healthy = payload
                .get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if format == "json" {
                CliRunResult {
                    exit_code: if healthy { 0 } else { 2 },
                    stdout: format!("{}\n", stable_json_dumps(&payload)),
                    stderr: String::new(),
                }
            } else {
                CliRunResult {
                    exit_code: if healthy { 0 } else { 2 },
                    stdout: doctor_text_from_payload(&payload),
                    stderr: String::new(),
                }
            }
        }
        "agent" => agent_command(&argv[1..]),
        "plugin" | "plug" => plugin_command(&argv[1..]),
        "github" => github_command(&argv[1..]),
        "pr" => pr_command(&argv[1..]),
        "debug" => debug_command(&argv[1..]),
        "db" => db_command(&argv[1..]),
        "upgrade" => lifecycle_command("upgrade", &argv[1..]),
        "uninstall" => lifecycle_command("uninstall", &argv[1..]),
        "acp" => acp_command(&argv[1..]),
        "import" => session_import(&argv[1..]),
        "export" => session_export(&argv[1..]),
        "generate" => generate_command(&argv[1..]),
        "console" => console_command(&argv[1..]),
        _ => CliRunResult {
            exit_code: 2,
            stdout: String::new(),
            stderr: format!("unsupported Rust CLI command: {}\n", argv[0]),
        },
    }
}

#[must_use]
pub fn run_cli_command_streaming(argv: &[String]) -> CliRunResult {
    if argv.first().map(String::as_str) == Some("run")
        && value_for(&argv[1..], &["--format"]).as_deref() == Some("json")
    {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        let mut emit = |event: &Value| {
            let _ = writeln!(handle, "{}", stable_json_dumps(event));
            let _ = handle.flush();
        };
        return run_prompt_command_with_events(&argv[1..], Some(&mut emit));
    }
    run_cli_command(argv)
}

#[must_use]
pub fn stable_json_dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => {
            if *flag {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Number(number) => number.to_string(),
        Value::String(text) => serde_json::to_string(text).expect("string serializes"),
        Value::Array(items) => items
            .iter()
            .map(stable_json_dumps)
            .collect::<Vec<_>>()
            .join(", ")
            .pipe(|inner| format!("[{inner}]")),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            keys.into_iter()
                .map(|key| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(key).expect("key serializes"),
                        stable_json_dumps(&object[key])
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
                .pipe(|inner| format!("{{{inner}}}"))
        }
    }
}

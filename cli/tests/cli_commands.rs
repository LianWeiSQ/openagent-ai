use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
    time::{SystemTime, UNIX_EPOCH},
};

use openagent_cli::cli_commands_fixture;
use openagent_lsp::command_available;
use openagent_protocol::{ChatMessage, Role};
use openagent_session::{FileSessionStore, Session, StartRunOptions};
use serde_json::{Value, json};

type MockServer = thread::JoinHandle<Result<(), String>>;
type CapturedRequests = Arc<Mutex<Vec<String>>>;

#[test]
fn cli_commands_fixture_matches_legacy_oracle() -> Result<(), Box<dyn Error>> {
    let fixture = read_fixture()?;
    assert_eq!(cli_commands_fixture(), fixture);
    Ok(())
}

#[test]
fn binary_default_smoke_prints_command_name() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_openagent")).output()?;
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout)?, "openagent\n");
    assert_eq!(String::from_utf8(output.stderr)?, "");
    Ok(())
}

#[test]
fn binary_skills_cli_lists_shows_and_doctors_workspace_skills() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-skills-command")?;
    let skill_dir = temp.join(".openagent/skills/rooted");
    fs::create_dir_all(&skill_dir)?;
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: rooted
description: Rooted CLI command skill
metadata:
  audience: test
---
Use rooted CLI command guidance.
"#,
    )?;

    let list = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "skills",
            "list",
            "--workspace",
            path_str(&temp),
            "--query",
            "root",
            "--limit",
            "1",
        ])
        .output()?;
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_payload: Value = serde_json::from_slice(&list.stdout)?;
    assert_eq!(list_payload["query"], "root");
    assert_eq!(list_payload["skills"][0]["name"], "rooted");
    assert_eq!(
        list_payload["skills"][0]["description"],
        "Rooted CLI command skill"
    );

    let show = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args(["skills", "show", "rooted", "--workspace", path_str(&temp)])
        .output()?;
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let show_payload: Value = serde_json::from_slice(&show.stdout)?;
    assert_eq!(show_payload["name"], "rooted");
    assert!(
        show_payload["rendered"]
            .as_str()
            .is_some_and(|rendered| rendered.contains("Use rooted CLI command guidance."))
    );

    let doctor = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args(["skills", "doctor", "--workspace", path_str(&temp)])
        .output()?;
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor_payload: Value = serde_json::from_slice(&doctor.stdout)?;
    assert!(doctor_payload["loaded_count"].as_u64().unwrap_or_default() >= 1);
    assert_eq!(doctor_payload["invalid_count"], 0);

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_lsp_cli_reports_status_and_queries_symbols() -> Result<(), Box<dyn Error>> {
    if !command_available("python3") {
        return Ok(());
    }
    let Some(python) = python3_executable()? else {
        return Ok(());
    };
    let temp = temp_dir("openagent-cli-lsp-command")?;
    fs::write(temp.join("Cargo.toml"), "[package]\nname = \"fake\"\n")?;
    fs::create_dir_all(temp.join("src"))?;
    fs::write(temp.join("src/main.rs"), "fn main() {}\n")?;
    let fake = write_fake_lsp_server(&temp)?;
    fs::create_dir_all(temp.join(".openagent"))?;
    fs::write(
        temp.join(".openagent/lsp.json"),
        serde_json::to_string_pretty(&json!({
            "servers": {
                "fake": {
                    "command": [python, fake],
                    "extensions": [".rs"],
                    "root_markers": ["Cargo.toml"]
                }
            }
        }))?,
    )?;

    let status = run_openagent(["lsp", "status", "--workspace", path_str(&temp)], None)?;
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_payload: Value = serde_json::from_slice(&status.stdout)?;
    let fake_available = status_payload["servers"].as_array().is_some_and(|servers| {
        servers
            .iter()
            .any(|server| server["id"] == "fake" && server["available"] == true)
    });
    assert!(fake_available);

    let query = run_openagent(
        [
            "lsp",
            "query",
            "documentSymbol",
            "src/main.rs",
            "--workspace",
            path_str(&temp),
            "--timeout-ms",
            "3000",
        ],
        None,
    )?;
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    let query_payload: Value = serde_json::from_slice(&query.stdout)?;
    assert_eq!(query_payload["server_id"], "fake");
    assert_eq!(query_payload["server_ids"], json!(["fake"]));
    assert_eq!(query_payload["result"][0]["name"], "main");

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_doctor_json_smoke_uses_environment() -> Result<(), Box<dyn Error>> {
    let (port, server) = serve_http_once_on_free_port(
        "application/json",
        json!({"data": [{"id": "gpt-test"}]}).to_string(),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args(["doctor", "--format", "json"])
        .env_clear()
        .env("OPENAI_API_KEY", "secret")
        .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("OPENAI_MODEL", "gpt-test")
        .env("OPENAI_WIRE_API", "responses")
        .output()?;
    assert!(output.status.success());
    server
        .join()
        .expect("doctor server thread")
        .expect("doctor response");
    let stdout = String::from_utf8(output.stdout)?;
    let payload: Value = serde_json::from_str(&stdout)?;
    assert_eq!(payload["provider"], "openai");
    assert_eq!(payload["base_url"], format!("http://127.0.0.1:{port}"));
    assert_eq!(payload["model_endpoint_ok"], true);
    assert_eq!(payload["configured_model_available"], true);
    assert!(
        payload["model_endpoint_message"]
            .as_str()
            .is_some_and(|message| message.contains("/models"))
    );
    assert!(!stdout.contains("secret"));
    Ok(())
}

#[test]
fn binary_doctor_json_respects_cli_model_overrides() -> Result<(), Box<dyn Error>> {
    let (port, server) = serve_http_once_on_free_port(
        "application/json",
        json!({"data": [{"id": "gpt-cli"}]}).to_string(),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "doctor",
            "--format",
            "json",
            "--base-url",
            &format!("http://127.0.0.1:{port}"),
            "--model",
            "gpt-cli",
            "--wire-api",
            "chat",
            "--api-key",
            "cli-secret",
        ])
        .env_clear()
        .env("OPENAI_BASE_URL", "http://env.test")
        .env("OPENAI_MODEL", "gpt-env")
        .env("OPENAI_WIRE_API", "responses")
        .output()?;
    assert!(output.status.success());
    server
        .join()
        .expect("doctor override server thread")
        .expect("doctor override response");
    let stdout = String::from_utf8(output.stdout)?;
    let payload: Value = serde_json::from_str(&stdout)?;
    assert_eq!(payload["base_url"], format!("http://127.0.0.1:{port}"));
    assert_eq!(payload["base_url_source"], "cli");
    assert_eq!(payload["model"], "gpt-cli");
    assert_eq!(payload["wire_api"], "chat");
    assert_eq!(payload["api_key_set"], true);
    assert!(!stdout.contains("cli-secret"));
    Ok(())
}

#[test]
fn binary_run_uses_auth_file_provider_config_without_skip_doctor() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-auth-provider-run")?;
    let auth_path = temp.join("auth.json");
    let (port, server) = serve_http_responses_on_free_port(vec![
        json_response_body(json!({"data": [{"id": "gpt-auth"}]})),
        json_response_body(json!({
            "choices": [{
                "message": {"content": "pong"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })),
    ])?;
    fs::write(
        &auth_path,
        serde_json::to_string_pretty(&json!({
            "providers": {
                "openai": {
                    "provider": "openai",
                    "type": "api",
                    "api_key": "auth-secret",
                    "base_url": format!("http://127.0.0.1:{port}"),
                    "model": "gpt-auth",
                    "wire_api": "chat"
                }
            }
        }))?,
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--auth-file",
            path_str(&auth_path),
            "--format",
            "json",
            "Reply",
            "pong",
        ])
        .env_clear()
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server
        .join()
        .expect("provider config server thread")
        .expect("provider config responses");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("auth-secret"));
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "pong"
            && event["params"]["source"] == "openai:chat"
    }));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_uses_native_gemini_payload_and_keeps_tool_controls_internal()
-> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-native-gemini")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("gemini-native.json"),
        serde_json::to_string_pretty(&json!({
            "id": "gemini-native",
            "name": "Gemini Native",
            "mode": "primary",
            "provider": "gemini",
            "model": "gemini-fixture",
            "tools": ["read"],
            "model_options": {
                "max_output_tokens": 32,
                "parallel_tool_calls": false,
                "tool_call_dialect": "native",
                "tool_choice": "required"
            }
        }))?,
    )?;
    let (provider_port, provider_server, provider_requests) = serve_http_capture_once_on_free_port(
        "application/json",
        json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "native gemini answer"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 3,
                "candidatesTokenCount": 2
            }
        })
        .to_string(),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&workspace),
            "--session-root",
            path_str(&session_root),
            "--agent",
            "gemini-native",
            "--format",
            "json",
            "Use",
            "Gemini",
        ])
        .env_clear()
        .env("GOOGLE_API_KEY", "gemini-secret")
        .env(
            "GOOGLE_BASE_URL",
            format!("http://127.0.0.1:{provider_port}"),
        )
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    provider_server
        .join()
        .expect("native Gemini server")
        .expect("native Gemini response");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("gemini-secret"));
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "native gemini answer"
            && event["params"]["source"] == "gemini:generate_content"
    }));

    let requests = provider_requests.lock().expect("Gemini request capture");
    assert_eq!(requests.len(), 1);
    let payload: Value = serde_json::from_str(&requests[0])?;
    assert_eq!(payload["contents"][0]["parts"][0]["text"], "Use Gemini");
    assert_eq!(
        payload["toolConfig"]["functionCallingConfig"]["mode"],
        "ANY"
    );
    assert_eq!(payload["generationConfig"]["maxOutputTokens"], 32);
    assert!(
        payload["tools"][0]["functionDeclarations"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "read"))
    );
    for internal_key in ["parallel_tool_calls", "tool_call_dialect", "tool_choice"] {
        assert!(payload.get(internal_key).is_none(), "{internal_key} leaked");
    }

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_native_tool_dialect_does_not_parse_xml_like_text() -> Result<(), Box<dyn Error>> {
    let xml_like_answer =
        r#"<tool_call>{"name":"read","arguments":{"file_path":"notes.txt"}}</tool_call>"#;
    let (provider_port, provider_server, provider_requests) = serve_http_capture_once_on_free_port(
        "application/json",
        json!({
            "choices": [{
                "message": {"role": "assistant", "content": xml_like_answer},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 3}
        })
        .to_string(),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--wire-api",
            "chat",
            "--format",
            "json",
            "Explain",
            "this",
            "text",
        ])
        .env_clear()
        .env("OPENAI_API_KEY", "test-key")
        .env(
            "OPENAI_BASE_URL",
            format!("http://127.0.0.1:{provider_port}"),
        )
        .env("OPENAI_MODEL", "chat-fixture")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    provider_server
        .join()
        .expect("native dialect server")
        .expect("native dialect response");
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == xml_like_answer
            && event["params"]["tool_calls"] == 0
    }));
    assert_eq!(
        provider_requests
            .lock()
            .expect("native dialect request capture")
            .len(),
        1
    );
    Ok(())
}

#[test]
fn binary_help_smoke_covers_legacy_command_surface() -> Result<(), Box<dyn Error>> {
    let root = run_openagent(["--help"], None)?;
    assert!(root.status.success());
    let root_stdout = String::from_utf8(root.stdout)?;
    for command in [
        "tui",
        "run",
        "serve",
        "client",
        "attach",
        "terminal",
        "session",
        "models",
        "stats",
        "command",
        "config",
        "auth",
        "providers",
        "mcp",
        "doctor",
    ] {
        assert!(
            root_stdout.contains(command),
            "root help should mention {command}"
        );
        let output = run_openagent([command, "--help"], None)?;
        assert!(
            output.status.success(),
            "{command} --help failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let web = run_openagent(["web", "--help"], None)?;
    assert!(!web.status.success());
    let web_stderr = String::from_utf8(web.stderr)?;
    assert!(web_stderr.contains("unsupported Rust CLI command: web"));

    let run_help = run_openagent(["run", "--help"], None)?;
    let run_stdout = String::from_utf8(run_help.stdout)?;
    for opencode_flag in [
        "--fork",
        "--share",
        "--agent",
        "--title",
        "--attach",
        "--variant",
        "--thinking",
        "--dangerously-skip-permissions",
    ] {
        assert!(
            run_stdout.contains(opencode_flag),
            "run help should expose OpenCode parity flag {opencode_flag}"
        );
    }
    Ok(())
}

#[test]
fn binary_terminal_runs_remote_bridge_command() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-cli-terminal")?;
    let workspace = temp.join("workspace");
    let nested = workspace.join("nested");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&nested)?;
    let mut server = spawn_openagent_server(port, &workspace, &session_root)?;
    wait_for_attach(port)?;

    let url = format!("http://127.0.0.1:{port}");
    let text = run_openagent_vec(vec![
        "terminal".to_string(),
        "--server-url".to_string(),
        url.clone(),
        "--server-token".to_string(),
        "secret".to_string(),
        "--command".to_string(),
        "printf cli-terminal-ok".to_string(),
    ])?;
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert_eq!(String::from_utf8(text.stdout)?, "cli-terminal-ok");

    let json_output = run_openagent_vec(vec![
        "terminal".to_string(),
        "--server-url".to_string(),
        url,
        "--server-token".to_string(),
        "secret".to_string(),
        "--cwd".to_string(),
        nested.to_string_lossy().to_string(),
        "--timeout-ms".to_string(),
        "5000".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--".to_string(),
        "pwd".to_string(),
    ])?;
    assert!(
        json_output.status.success(),
        "{}",
        String::from_utf8_lossy(&json_output.stderr)
    );
    let payload: Value = serde_json::from_slice(&json_output.stdout)?;
    assert_eq!(payload["success"], true);
    assert_eq!(payload["cwd_relative"], "nested");
    assert_eq!(payload["exit_code"], 0);
    assert!(
        payload["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("nested"))
    );

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_mcp_remote_lifecycle_controls_bridge() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-cli-remote-mcp")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    let openagent_dir = workspace.join(".openagent");
    fs::create_dir_all(&openagent_dir)?;
    fs::create_dir_all(&session_root)?;
    let server_script = temp.join("stdio_mcp_server.py");
    fs::write(&server_script, stdio_mcp_server_script())?;
    fs::write(
        openagent_dir.join("mcp.json"),
        format!(
            r#"{{
              "mcpServers": {{
                "local-tools": {{
                  "command": "python3",
                  "args": ["{}"],
                  "enabled": false,
                  "timeout_ms": 5000
                }}
              }}
            }}"#,
            server_script.display()
        ),
    )?;
    let mut server = spawn_openagent_server(port, &workspace, &session_root)?;
    wait_for_attach(port)?;

    let url = format!("http://127.0.0.1:{port}");
    let base = vec![
        "--server-url".to_string(),
        url.clone(),
        "--server-token".to_string(),
        "secret".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];

    let mut list_args = vec!["mcp".to_string(), "list".to_string()];
    list_args.extend(base.clone());
    let list = run_openagent_vec(list_args)?;
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_payload: Value = serde_json::from_slice(&list.stdout)?;
    assert_eq!(list_payload["remote"], true);
    assert_eq!(list_payload["servers"][0]["name"], "local-tools");
    assert_eq!(list_payload["servers"][0]["enabled"], false);
    assert_eq!(list_payload["servers"][0]["lifecycle_status"], "stopped");

    let mut start_args = vec![
        "mcp".to_string(),
        "start".to_string(),
        "local-tools".to_string(),
    ];
    start_args.extend(base.clone());
    let start = run_openagent_vec(start_args)?;
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let start_payload: Value = serde_json::from_slice(&start.stdout)?;
    assert_eq!(start_payload["servers"][0]["lifecycle_status"], "running");
    assert_eq!(start_payload["servers"][0]["enabled"], false);
    let lifecycle_pid = start_payload["servers"][0]["lifecycle_pid"]
        .as_u64()
        .ok_or("missing lifecycle pid")?;

    let mut enable_args = vec![
        "mcp".to_string(),
        "enable".to_string(),
        "local-tools".to_string(),
    ];
    enable_args.extend(base.clone());
    let enable = run_openagent_vec(enable_args)?;
    assert!(
        enable.status.success(),
        "{}",
        String::from_utf8_lossy(&enable.stderr)
    );
    let enable_payload: Value = serde_json::from_slice(&enable.stdout)?;
    assert_eq!(enable_payload["servers"][0]["enabled"], true);
    assert_eq!(enable_payload["servers"][0]["lifecycle_status"], "running");
    assert_eq!(
        enable_payload["servers"][0]["lifecycle_pid"],
        json!(lifecycle_pid)
    );

    let mut test_args = vec![
        "mcp".to_string(),
        "test".to_string(),
        "local-tools".to_string(),
    ];
    test_args.extend(base.clone());
    let test = run_openagent_vec(test_args)?;
    assert!(
        test.status.success(),
        "{}",
        String::from_utf8_lossy(&test.stderr)
    );
    let test_payload: Value = serde_json::from_slice(&test.stdout)?;
    assert_eq!(test_payload["servers"][0]["tool_count"], 1);
    assert_eq!(test_payload["servers"][0]["selected_transport"], "stdio");
    assert_eq!(
        test_payload["servers"][0]["lifecycle_pid"],
        json!(lifecycle_pid)
    );

    let mut stop_args = vec![
        "mcp".to_string(),
        "stop".to_string(),
        "local-tools".to_string(),
    ];
    stop_args.extend(base);
    let stop = run_openagent_vec(stop_args)?;
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let stop_payload: Value = serde_json::from_slice(&stop.stdout)?;
    assert_eq!(stop_payload["servers"][0]["lifecycle_status"], "stopped");

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_mcp_test_uses_local_mcp_config_alias_once() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-mcp-test-alias")?;
    let workspace = temp.join("workspace");
    fs::create_dir_all(&workspace)?;
    let mcp_config = temp.join("mcp.json");
    let server_script = temp.join("stdio_mcp_server.py");
    fs::write(&server_script, stdio_mcp_server_script())?;
    fs::write(
        &mcp_config,
        format!(
            r#"{{
              "mcpServers": {{
                "local-tools": {{
                  "command": "python3",
                  "args": ["{}"],
                  "enabled": false,
                  "timeout_ms": 5000
                }}
              }}
            }}"#,
            server_script.display()
        ),
    )?;

    let output = run_openagent_vec(vec![
        "mcp".to_string(),
        "test".to_string(),
        "local-tools".to_string(),
        "--mcp-config".to_string(),
        path_str(&mcp_config).to_string(),
        "--workspace".to_string(),
        path_str(&workspace).to_string(),
        "--format".to_string(),
        "json".to_string(),
    ])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["name"], "local-tools");
    assert_eq!(payload["server"]["status"], "connected");
    assert_eq!(payload["server"]["selected_transport"], "stdio");
    assert_eq!(payload["server"]["tool_count"], 1);
    assert_eq!(
        payload["config_path"].as_str().unwrap_or_default(),
        path_str(&mcp_config)
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_mcp_lifecycle_rejects_local_config_without_bridge() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-mcp-lifecycle-local-config")?;
    let mcp_config = temp.join("mcp.json");
    fs::write(
        &mcp_config,
        r#"{"mcpServers":{"local-tools":{"command":"python3","args":["server.py"]}}}"#,
    )?;

    let output = run_openagent_vec(vec![
        "mcp".to_string(),
        "start".to_string(),
        "local-tools".to_string(),
        "--mcp-config".to_string(),
        path_str(&mcp_config).to_string(),
    ])?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Bridge lifecycle registry"), "{stderr}");
    assert!(stderr.contains("--server-url <url>"), "{stderr}");
    assert!(!stderr.contains("Connection refused"), "{stderr}");

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_and_models_smokes_are_machine_readable() -> Result<(), Box<dyn Error>> {
    let run = run_openagent(
        ["run", "--skip-doctor", "--format", "json", "hello", "agent"],
        None,
    )?;
    assert!(run.status.success());
    let events = String::from_utf8(run.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(events[0]["method"], "item/agentMessage/delta");
    assert_eq!(events[1]["method"], "turn/completed");

    let models = run_openagent(["models", "--format", "json"], None)?;
    assert!(models.status.success());
    let payload: Value = serde_json::from_slice(&models.stdout)?;
    assert_eq!(payload["provider"], "openai");
    assert_eq!(payload["models"][0]["id"], "gpt-5.5");
    Ok(())
}

#[test]
fn binary_run_does_not_leak_flag_values_into_prompt() -> Result<(), Box<dyn Error>> {
    let run = run_openagent(
        [
            "run",
            "--skip-doctor",
            "--base-url",
            "http://private-gateway.test",
            "--model",
            "gpt-private",
            "--api-key",
            "private-key",
            "--max-steps",
            "2",
            "--format",
            "json",
            "hello",
        ],
        None,
    )?;
    assert!(run.status.success());
    let stdout = String::from_utf8(run.stdout)?;
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(events[0]["params"]["prompt"], "hello");
    assert!(!stdout.contains("private-gateway"));
    assert!(!stdout.contains("gpt-private"));
    assert!(!stdout.contains("private-key"));
    Ok(())
}

#[test]
fn binary_models_uses_provider_specific_model_environment() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args(["models", "anthropic", "--format", "json"])
        .env_clear()
        .env("OPENAI_MODEL", "gpt-env")
        .env("ANTHROPIC_MODEL", "claude-env")
        .output()?;
    assert!(output.status.success());
    let payload: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(payload["provider"], "anthropic");
    assert_eq!(payload["models"][0]["id"], "claude-env");
    Ok(())
}

#[test]
fn binary_config_auth_and_mcp_file_flows_work_without_python() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-flow")?;
    let config_path = temp.join("openagent.env");
    let auth_path = temp.join("auth.json");
    let mcp_path = temp.join("mcp.json");

    let config = run_openagent(
        [
            "config",
            "init",
            "--path",
            path_str(&config_path),
            "--api-key",
            "secret-key",
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(config.status.success());
    let config_payload: Value = serde_json::from_slice(&config.stdout)?;
    assert_eq!(config_payload["created"], true);
    assert!(fs::read_to_string(&config_path)?.contains("OPENAI_API_KEY=secret-key"));

    let login = run_openagent(
        [
            "auth",
            "login",
            "--auth-file",
            path_str(&auth_path),
            "--provider",
            "groq",
            "--api-key",
            "groq-secret",
            "--base-url",
            "https://api.groq.example/v1",
            "--model",
            "llama-fixture",
        ],
        None,
    )?;
    assert!(login.status.success());
    let login_payload: Value = serde_json::from_slice(&login.stdout)?;
    assert_eq!(login_payload["status"], "logged_in");
    assert!(!String::from_utf8(login.stdout)?.contains("groq-secret"));

    let list = run_openagent(
        [
            "providers",
            "list",
            "--auth-file",
            path_str(&auth_path),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(list.status.success());
    let list_payload: Value = serde_json::from_slice(&list.stdout)?;
    assert_eq!(list_payload["providers"][0]["provider"], "groq");

    let mcp_add = run_openagent(
        [
            "mcp",
            "add",
            "demo",
            "--config",
            path_str(&mcp_path),
            "--url",
            "https://user:password@example.com/mcp?token=secret&safe=1",
            "--header",
            "Authorization=Bearer private",
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(mcp_add.status.success());
    let add_payload: Value = serde_json::from_slice(&mcp_add.stdout)?;
    assert_eq!(add_payload["updated"], true);
    let add_stdout = String::from_utf8(mcp_add.stdout)?;
    assert!(!add_stdout.contains("password"));
    assert!(!add_stdout.contains("secret&"));
    assert!(!add_stdout.contains("Bearer private"));

    let doctor = run_openagent(
        [
            "mcp",
            "doctor",
            "--config",
            path_str(&mcp_path),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(doctor.status.success());
    let doctor_payload: Value = serde_json::from_slice(&doctor.stdout)?;
    assert_eq!(doctor_payload["server_count"], 1);

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_session_checkpoints_and_restore_revert_workspace_and_transcript()
-> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-session-restore")?;
    let session_root = temp.join("sessions");
    let workspace = temp.join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("note.txt"), "before")?;

    let store = FileSessionStore::new(&session_root);
    let mut session = Session::new("restore_session", &workspace);
    store
        .start_run(
            &mut session,
            StartRunOptions {
                run_id: "restore_run".to_string(),
                trace_id: "restore_trace".to_string(),
                agent_name: "agent".to_string(),
                model_id: Some("model".to_string()),
                provider_id: Some("provider".to_string()),
                permission: "FULL".to_string(),
                max_steps: 3,
                started_at_ms: Some(1),
            },
        )
        .expect("run starts");
    let first = ChatMessage {
        role: Role::User,
        content: "first".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_restore_1"))]),
    };
    session.add(first.clone());
    store
        .append_message(&session, &first, "restore_run", 0)
        .expect("first message appends");
    let checkpoint = store
        .create_checkpoint(
            "restore_session",
            "restore_run",
            &workspace,
            "manual",
            Some("msg_restore_1"),
            None,
            Some(1),
        )
        .expect("checkpoint creates");

    fs::write(workspace.join("note.txt"), "after")?;
    fs::write(workspace.join("new.txt"), "new")?;
    let second = ChatMessage {
        role: Role::User,
        content: "second".to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!("msg_restore_2"))]),
    };
    session.add(second.clone());
    store
        .append_message(&session, &second, "restore_run", 1)
        .expect("second message appends");
    store
        .save_state(&session, Some("restore_run"))
        .expect("state saves");

    let checkpoints = run_openagent(
        [
            "session",
            "checkpoints",
            "restore_session",
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(checkpoints.status.success());
    let checkpoints_payload: Value = serde_json::from_slice(&checkpoints.stdout)?;
    assert_eq!(checkpoints_payload["checkpoint_count"], 1);
    assert_eq!(
        checkpoints_payload["checkpoints"][0]["checkpoint_id"],
        checkpoint.checkpoint_id
    );

    let restore = run_openagent(
        [
            "session",
            "restore",
            "restore_session",
            &checkpoint.checkpoint_id,
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(
        restore.status.success(),
        "{}",
        String::from_utf8(restore.stderr)?
    );
    let restore_payload: Value = serde_json::from_slice(&restore.stdout)?;
    assert_eq!(restore_payload["restored"], true);
    assert_eq!(fs::read_to_string(workspace.join("note.txt"))?, "before");
    assert!(!workspace.join("new.txt").exists());

    let restored_messages = store
        .list_messages_with_parts("restore_session", None, None)
        .expect("restored messages load");
    assert_eq!(restored_messages.len(), 1);
    assert_eq!(restored_messages[0].info.id, "msg_restore_1");

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_models_catalog_and_backlog_commands_are_deep_local_workflows()
-> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-deep-workflows")?;
    let models_cache = temp.join("models.json");
    let models_body = r#"{
      "openai": {
        "id": "openai",
        "name": "OpenAI",
        "api": "https://api.openai.com/v1",
        "doc": "https://platform.openai.com/docs",
        "env": ["OPENAI_API_KEY"],
        "models": {
          "openai/gpt-test": {
            "id": "openai/gpt-test",
            "name": "GPT Test",
            "family": "gpt",
            "attachment": true,
            "reasoning": true,
            "tool_call": true,
            "structured_output": true,
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "limit": {"context": 128000, "output": 16384},
            "cost": {"input": 1.25, "output": 10, "cache_read": 0.125}
          }
        }
      },
      "google": {
        "id": "google",
        "name": "Google",
        "models": {
          "google/gemini-test": {
            "id": "google/gemini-test",
            "name": "Gemini Test",
            "reasoning": true,
            "tool_call": true,
            "modalities": {"input": ["text", "image", "pdf"], "output": ["text"]},
            "limit": {"context": 1048576, "output": 65536},
            "cost": {"input": 2, "output": 12}
          }
        }
      }
    }"#;
    let (port, server) = serve_http_once_on_free_port("application/json", models_body.to_string())?;
    let models = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "models",
            "openai",
            "--refresh",
            "--models-url",
            &format!("http://127.0.0.1:{port}"),
            "--ttl-seconds",
            "3600",
            "--format",
            "json",
        ])
        .env_clear()
        .env("OPENAGENT_MODELS_PATH", path_str(&models_cache))
        .output()?;
    assert!(
        models.status.success(),
        "{}",
        String::from_utf8_lossy(&models.stderr)
    );
    server
        .join()
        .expect("models server thread")
        .expect("models response");
    let payload: Value = serde_json::from_slice(&models.stdout)?;
    assert_eq!(payload["cache"]["status"], "refreshed");
    assert_eq!(payload["models"][0]["id"], "openai/gpt-test");
    assert_eq!(payload["models"][0]["provider_model_id"], "gpt-test");
    assert_eq!(payload["models"][0]["capabilities"]["vision"], true);

    let catalog = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args(["models", "--catalog", "--offline", "--format", "json"])
        .env_clear()
        .env("OPENAGENT_MODELS_PATH", path_str(&models_cache))
        .output()?;
    assert!(catalog.status.success());
    let catalog_payload: Value = serde_json::from_slice(&catalog.stdout)?;
    assert!(
        catalog_payload["providers"]
            .as_array()
            .is_some_and(|items| { items.iter().any(|item| item["id"] == "gemini") })
    );

    fs::remove_file(&models_cache)?;
    let fallback = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "models",
            "openai",
            "--refresh",
            "--models-url",
            "http://127.0.0.1:9",
            "--format",
            "json",
        ])
        .env_clear()
        .env("OPENAGENT_MODELS_PATH", path_str(&models_cache))
        .output()?;
    assert!(fallback.status.success());
    let fallback_payload: Value = serde_json::from_slice(&fallback.stdout)?;
    assert_eq!(fallback_payload["fallback"], true);
    assert_eq!(fallback_payload["cache"]["status"], "snapshot_fallback");

    let plugin_dir = temp.join("demo-plugin");
    fs::create_dir_all(plugin_dir.join(".codex-plugin"))?;
    fs::write(
        plugin_dir.join(".codex-plugin/plugin.json"),
        r#"{"id":"demo-plugin","name":"Demo Plugin","commands":{"default":{"description":"demo"}}}"#,
    )?;
    let plugin = run_openagent(
        [
            "plugin",
            "install",
            path_str(&plugin_dir),
            "--workspace",
            path_str(&temp),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(plugin.status.success());
    let plugin_payload: Value = serde_json::from_slice(&plugin.stdout)?;
    assert_eq!(plugin_payload["plugin_id"], "demo-plugin");

    let workflow = run_openagent(
        [
            "github",
            "workflow",
            "123",
            "--workspace",
            path_str(&temp),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(workflow.status.success());
    let workflow_payload: Value = serde_json::from_slice(&workflow.stdout)?;
    assert_eq!(workflow_payload["workflow"]["branch"], "openagent/123");

    let session_root = temp.join("sessions");
    fs::create_dir_all(session_root.join("s1/runs/r1"))?;
    fs::write(
        session_root.join("s1/state.latest.json"),
        r#"{"session_id":"s1","workspace":"alpha-workspace","status":"idle","updated_at_ms":10,"messages":[{"role":"user","content":"hi"}]}"#,
    )?;
    let db = run_openagent(
        [
            "db",
            "rebuild",
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(db.status.success());
    let db_payload: Value = serde_json::from_slice(&db.stdout)?;
    assert_eq!(db_payload["rows"], 1);
    let query = run_openagent(
        [
            "db",
            "query",
            "alpha",
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(query.status.success());
    let query_payload: Value = serde_json::from_slice(&query.stdout)?;
    assert_eq!(query_payload["rows"].as_array().map_or(0, Vec::len), 1);

    let generate = run_openagent(["generate", "commands"], None)?;
    assert!(generate.status.success());
    let generate_payload: Value = serde_json::from_slice(&generate.stdout)?;
    assert!(
        generate_payload["commands"]
            .as_array()
            .is_some_and(|items| { items.iter().any(|item| item == "plugin") })
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_streams_openai_chat_sse_provider_events() -> Result<(), Box<dyn Error>> {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello \"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"streamed\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let (port, server) = serve_http_once_on_free_port("text/event-stream", body)?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--provider",
            "openai",
            "--api-key",
            "secret",
            "--base-url",
            &format!("http://127.0.0.1:{port}"),
            "--wire-api",
            "chat",
            "--stream",
            "--format",
            "json",
            "hello",
        ])
        .env_clear()
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server
        .join()
        .expect("provider server thread")
        .expect("provider response");
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(events.iter().any(|event| {
        event["method"] == "item/agentMessage/delta"
            && event["params"]["delta"]
                .as_str()
                .is_some_and(|text| text.contains("hello ") || text.contains("streamed"))
    }));
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed" && event["params"]["source"] == "openai:chat:stream"
    }));
    Ok(())
}

#[test]
fn binary_run_emits_provider_sse_delta_before_stream_closes() -> Result<(), Box<dyn Error>> {
    let (port, server) = serve_dripping_sse_provider()?;
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--provider",
            "openai",
            "--api-key",
            "secret",
            "--base-url",
            &format!("http://127.0.0.1:{port}"),
            "--wire-api",
            "chat",
            "--stream",
            "--format",
            "json",
            "hello",
        ])
        .env_clear()
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("missing child stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "first stream event should arrive before the mock server closes"
    );
    let first_event: Value = serde_json::from_str(first_line.trim())?;
    assert_eq!(first_event["method"], "item/agentMessage/delta");
    assert_eq!(first_event["params"]["delta"], "hello ");

    let mut rest = String::new();
    reader.read_to_string(&mut rest)?;
    let status = child.wait()?;
    assert!(status.success());
    server
        .join()
        .expect("provider server thread")
        .expect("provider response");
    let events = rest
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(events.iter().any(|event| {
        event["method"] == "item/agentMessage/delta" && event["params"]["delta"] == "streamed"
    }));
    assert!(
        events
            .iter()
            .any(|event| event["method"] == "turn/completed")
    );
    Ok(())
}

#[test]
fn binary_run_executes_mock_tool_loop() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-agent-loop")?;
    fs::write(temp.join("notes.txt"), "alpha\nbeta\n")?;
    let session_root = temp.join("sessions");
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
            "read",
            "notes",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_read","name":"read","input":{"file_path":"notes.txt"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "final answer")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        events
            .iter()
            .any(|event| event["method"] == "item/toolCall/started")
    );
    assert!(
        events
            .iter()
            .any(|event| event["method"] == "item/toolCall/completed")
    );
    let completed = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .expect("completion event");
    assert_eq!(completed["params"]["final_answer"], "final answer");
    assert_eq!(completed["params"]["steps"], 2);
    assert_eq!(completed["params"]["tool_calls"], 1);

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_local_run_builds_structured_context_pack_for_every_source() -> Result<(), Box<dyn Error>>
{
    let temp = temp_dir("openagent-cli-context-pack")?;
    let workspace = temp.join("workspace");
    fs::create_dir_all(&workspace)?;
    let session_root = temp.join("sessions");
    fs::write(workspace.join("context.txt"), "typed local attachment\n")?;
    let skill_dir = workspace.join("shared-skills/brief");
    fs::create_dir_all(&skill_dir)?;
    fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: brief
description: Structured context test skill
---
Use the structured context test guidance.
"#,
    )?;
    let agent_dir = workspace.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("unified.json"),
        serde_json::to_string_pretty(&json!({
            "id": "unified",
            "name": "Unified Context",
            "mode": "primary",
            "prompt": "You are the unified context test agent.",
            "skills": ["brief"],
            "skill_roots": ["shared-skills"],
            "tools": ["todowrite", "mcp_tool_demo_echo"],
            "temperature": 0.2,
            "top_p": 0.8,
            "model_options": {
                "frequency_penalty": 0.1
            }
        }))?,
    )?;
    let mcp_config = workspace.join("mcp.json");
    let (port, server) = serve_mcp_json_rpc(1)?;
    fs::write(
        &mcp_config,
        format!(
            r#"{{
              "mcp": {{
                "demo": {{
                  "type": "remote",
                  "transport": "http",
                  "url": "http://127.0.0.1:{port}",
                  "enabled": true
                }}
              }}
            }}"#
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&workspace),
            "--session-root",
            path_str(&session_root),
            "--mcp-config",
            path_str(&mcp_config),
            "--agent",
            "unified",
            "--file",
            "context.txt",
            "--variant",
            "high",
            "--max-output-tokens",
            "2048",
            "--format",
            "json",
            "inspect",
            "context",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_todo","name":"todowrite","input":{"todos":[{"id":"todo-1","content":"Inspect every context source","status":"in_progress","priority":"high"}]}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "context complete")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server
        .join()
        .expect("mcp server thread")
        .expect("mcp discovery response");
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .ok_or("missing context completion")?;
    let session_id = completed["params"]["session_id"]
        .as_str()
        .ok_or("missing context session id")?;
    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(session_id).join("state.latest.json"),
    )?)?;
    let user = state["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|message| message["role"] == "user"))
        .ok_or("missing context user message")?;
    assert_eq!(user["content"], "inspect context");
    assert!(
        !user["content"]
            .as_str()
            .unwrap_or_default()
            .contains("typed local attachment")
    );
    assert_eq!(
        user["metadata"]["context_attachments"][0]["content"],
        "typed local attachment\n"
    );
    let context = &state["metadata"]["context_pack"];
    assert_eq!(context["mode"], "active");
    assert_eq!(context["surface"], "cli");
    assert_eq!(context["step"], 2);
    assert_eq!(context["receipt"]["item_kind_counts"]["attachment_file"], 1);
    assert_eq!(context["receipt"]["item_kind_counts"]["skill_preloaded"], 1);
    assert_eq!(
        context["receipt"]["item_kind_counts"]["mcp_tool_manifest"],
        1
    );
    assert_eq!(context["receipt"]["item_kind_counts"]["todo"], 1);
    assert!(
        context["receipt"]["item_kind_counts"]["checkpoint"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );
    assert_eq!(
        context["receipt"]["tool_names"],
        json!(["mcp_tool_demo_echo", "todowrite"])
    );
    assert_eq!(
        context["receipt"]["model_option_keys"],
        json!([
            "frequency_penalty",
            "max_output_tokens",
            "reasoning_effort",
            "temperature",
            "top_p"
        ])
    );
    assert_eq!(state["todos"][0]["id"], "todo-1");
    assert_eq!(state["todos"][0]["status"], "in_progress");

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_agent_registry_exposes_builtin_subagents() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-builtin-agents")?;
    let list = run_openagent(
        [
            "agent",
            "list",
            "--workspace",
            path_str(&temp),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let payload: Value = serde_json::from_slice(&list.stdout)?;
    let agents = payload["agents"].as_array().ok_or("missing agents")?;
    for id in ["build", "general", "explore", "scout", "plan"] {
        assert!(agents.iter().any(|agent| agent["id"] == id), "missing {id}");
    }

    let show = run_openagent(
        [
            "agent",
            "show",
            "explore",
            "--workspace",
            path_str(&temp),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let explore: Value = serde_json::from_slice(&show.stdout)?;
    assert_eq!(explore["id"], "explore");
    assert_eq!(explore["mode"], "subagent");
    assert_eq!(explore["permission"], "READONLY");
    assert!(
        explore["description"]
            .as_str()
            .is_some_and(|value| value.contains("Read-only"))
    );
    let scout = agents
        .iter()
        .find(|agent| agent["id"] == "scout")
        .ok_or("missing scout")?;
    assert_eq!(scout["permission"], "READONLY");
    assert_eq!(
        scout["tools"],
        json!([
            "web_fetch",
            "read",
            "glob",
            "grep",
            "ls",
            "lsp",
            "code_search",
            "skill",
            "todoread"
        ])
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_agent_registry_loads_opencode_markdown_agents() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-opencode-agent-md")?;
    let session_root = temp.join("sessions");
    let agent_dir = temp.join(".opencode/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("markdown-research.md"),
        r#"---
id: markdown-research
name: Markdown Research
description: OpenCode markdown research agent
mode: subagent
permission: READONLY
tools:
  - read
skills:
  - brief
skill_roots:
  - shared-skills
model: markdown-child-model
steps: 2
temperature: 0.31
top_p: 0.73
reasoning_effort: medium
color: cyan
---
You are the CLI Markdown research subagent.
"#,
    )?;
    let shared_skill = temp.join("shared-skills/brief");
    fs::create_dir_all(&shared_skill)?;
    fs::write(
        shared_skill.join("SKILL.md"),
        r#"---
name: brief
description: Brief preloaded subagent skill
---
Use preloaded brief guidance.
"#,
    )?;
    fs::write(
        agent_dir.join("disabled-worker.md"),
        r#"---
id: disabled-worker
name: Disabled Worker
mode: subagent
disable: true
---
Disabled prompt.
"#,
    )?;

    let list = run_openagent(
        [
            "agent",
            "list",
            "--workspace",
            path_str(&temp),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let payload: Value = serde_json::from_slice(&list.stdout)?;
    let agents = payload["agents"].as_array().ok_or("missing agents")?;
    let markdown = agents
        .iter()
        .find(|agent| agent["id"] == "markdown-research")
        .ok_or("missing markdown agent")?;
    assert_eq!(markdown["name"], "Markdown Research");
    assert_eq!(markdown["steps"], 2);
    assert_eq!(markdown["temperature"], 0.31);
    assert_eq!(markdown["top_p"], 0.73);
    assert_eq!(markdown["color"], "cyan");
    assert_eq!(markdown["skills"], json!(["brief"]));
    assert_eq!(markdown["skill_roots"], json!(["shared-skills"]));
    assert_eq!(markdown["model_options"]["reasoning_effort"], "medium");
    assert!(!agents.iter().any(|agent| agent["id"] == "disabled-worker"));

    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--permission",
            "FULL",
            "--format",
            "json",
            "delegate",
            "markdown",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_markdown","name":"task","input":{"description":"Markdown task","prompt":"Run the markdown subagent.","subagent_type":"markdown-research"}}]"#,
        )
        .env("OPENAGENT_MOCK_SUBAGENT_ANSWER", "markdown child answer")
        .env("OPENAGENT_MOCK_ANSWER", "parent final")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout_text = String::from_utf8(output.stdout)?;
    assert!(!stdout_text.contains("Use preloaded brief guidance."));
    let events = stdout_text
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing markdown task completion")?;
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    assert_eq!(
        completed["params"]["metadata"]["model_options"]["reasoning_effort"],
        "medium"
    );
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        !messages.iter().any(|message| {
            message["role"] == "system"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains("You are the CLI Markdown research subagent.")
                        && content.contains("<preloaded_skills>")
                        && content.contains("Use preloaded brief guidance.")
                })
        })
    }));
    assert_eq!(child_state["metadata"]["skills"], json!(["brief"]));
    assert_eq!(
        child_state["metadata"]["preloaded_skills"],
        json!(["brief"])
    );
    assert_eq!(child_state["metadata"]["temperature"], 0.31);
    assert_eq!(child_state["metadata"]["top_p"], 0.73);
    assert_eq!(
        child_state["metadata"]["model_options"]["reasoning_effort"],
        "medium"
    );
    assert_eq!(child_state["metadata"]["color"], "cyan");

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_executes_task_subagent_tool() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-task-subagent")?;
    let session_root = temp.join("sessions");
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--permission",
            "FULL",
            "--format",
            "json",
            "delegate",
            "this",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_task","name":"task","input":{"description":"Explore fixture","prompt":"Find the important files and summarize them.","subagent_type":"explore"}}]"#,
        )
        .env("OPENAGENT_MOCK_SUBAGENT_ANSWER", "child answer")
        .env("OPENAGENT_MOCK_ANSWER", "parent final")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let tool_completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing task completion")?;
    assert_eq!(
        tool_completed["params"]["metadata"]["subagent_type"],
        "explore"
    );
    assert!(
        tool_completed["params"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("<task id=") && output.contains("child answer"))
    );
    let child_session_id = tool_completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let completed = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .ok_or("missing completion event")?;
    assert_eq!(completed["params"]["final_answer"], "parent final");
    assert_eq!(completed["params"]["steps"], 2);
    assert_eq!(completed["params"]["tool_calls"], 1);
    let parent_session_id = completed["params"]["session_id"]
        .as_str()
        .ok_or("missing parent session id")?;

    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["subagent"], true);
    assert_eq!(
        child_state["metadata"]["parent_session_id"],
        parent_session_id
    );
    assert_eq!(child_state["metadata"]["parent_tool_call_id"], "call_task");
    assert_eq!(child_state["metadata"]["agent_profile"]["id"], "explore");
    assert!(child_state["messages"].as_array().is_some_and(|messages| {
        !messages.iter().any(|message| {
            message["role"] == "system" && message["metadata"]["agent_profile"] == "explore"
        }) && messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"] == "Find the important files and summarize them."
        })
    }));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_enforces_agent_task_permissions() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-task-permissions")?;
    let session_root = temp.join("sessions");
    let agent_dir = temp.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("limited-build.json"),
        serde_json::to_string_pretty(&json!({
            "id": "limited-build",
            "name": "Limited Build",
            "description": "Primary agent that can only launch allowed-worker.",
            "mode": "primary",
            "permission": {
                "ruleset": "FULL",
                "task": {
                    "*": "deny",
                    "allowed-worker": "allow"
                }
            },
            "tools": ["task"]
        }))?,
    )?;
    for id in ["allowed-worker", "blocked-worker"] {
        fs::write(
            agent_dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&json!({
                "id": id,
                "name": id,
                "description": format!("{id} subagent"),
                "mode": "subagent",
                "permission": "READONLY",
                "prompt": format!("You are {id}."),
                "tools": ["read"],
                "max_steps": 2
            }))?,
        )?;
    }

    let denied = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--agent",
            "limited-build",
            "--permission",
            "FULL",
            "--format",
            "json",
            "try",
            "blocked",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_blocked","name":"task","input":{"description":"Blocked task","prompt":"Should not run.","subagent_type":"blocked-worker"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "parent handled denial")
        .output()?;
    assert!(
        denied.status.success(),
        "{}",
        String::from_utf8_lossy(&denied.stderr)
    );
    let denied_events = String::from_utf8(denied.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let failed = denied_events
        .iter()
        .find(|event| event["method"] == "item/toolCall/failed")
        .ok_or("missing denied task failure")?;
    assert_eq!(failed["params"]["name"], "task");
    assert_eq!(failed["params"]["metadata"]["permission_action"], "deny");
    assert_eq!(
        failed["params"]["metadata"]["permission_pattern"],
        "blocked-worker"
    );
    assert!(
        !session_root.exists()
            || !fs::read_dir(&session_root)?.flatten().any(|entry| {
                let state_path = entry.path().join("state.latest.json");
                let Ok(raw) = fs::read_to_string(state_path) else {
                    return false;
                };
                let Ok(state) = serde_json::from_str::<Value>(&raw) else {
                    return false;
                };
                state["metadata"]["subagent"].as_bool().unwrap_or(false)
            })
    );

    let allowed = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--agent",
            "limited-build",
            "--permission",
            "FULL",
            "--format",
            "json",
            "try",
            "allowed",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_allowed","name":"task","input":{"description":"Allowed task","prompt":"Run allowed.","subagent_type":"allowed-worker"}}]"#,
        )
        .env("OPENAGENT_MOCK_SUBAGENT_ANSWER", "allowed child answer")
        .env("OPENAGENT_MOCK_ANSWER", "parent handled allowed")
        .output()?;
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    let allowed_events = String::from_utf8(allowed.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = allowed_events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing allowed task completion")?;
    assert_eq!(
        completed["params"]["metadata"]["subagent_type"],
        "allowed-worker"
    );
    assert!(
        completed["params"]["output"]
            .as_str()
            .is_some_and(|output| {
                output.contains("<task id=") && output.contains("allowed child answer")
            })
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_invokes_subagent_with_at_mention() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-at-subagent")?;
    let session_root = temp.join("sessions");
    let agent_dir = temp.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("allowed-worker.json"),
        serde_json::to_string_pretty(&json!({
            "id": "allowed-worker",
            "name": "Allowed Worker",
            "description": "Manual at-mention worker",
            "mode": "subagent",
            "permission": "READONLY",
            "prompt": "You are the manual worker.",
            "tools": ["read"],
            "max_steps": 2
        }))?,
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--permission",
            "FULL",
            "--format",
            "json",
            "@allowed-worker",
            "Handle this directly.",
        ])
        .env_clear()
        .env("OPENAGENT_MOCK_SUBAGENT_ANSWER", "manual child answer")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing manual task completion")?;
    assert_eq!(completed["params"]["manual"], true);
    assert_eq!(
        completed["params"]["metadata"]["subagent_type"],
        "allowed-worker"
    );
    assert!(
        completed["params"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("manual child answer"))
    );
    let turn = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .ok_or("missing completed turn")?;
    assert_eq!(turn["params"]["source"], "manual_subagent");
    assert_eq!(turn["params"]["tool_calls"], 1);

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_auto_routes_prompt_to_matching_subagent_description() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-auto-subagent")?;
    let session_root = temp.join("sessions");
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--permission",
            "FULL",
            "--format",
            "json",
            "Research",
            "external",
            "dependency",
            "docs",
            "before",
            "coding.",
        ])
        .env_clear()
        .env("OPENAGENT_MOCK_SUBAGENT_ANSWER", "auto scout child answer")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or("missing auto task completion")?;
    assert_eq!(completed["params"]["manual"], false);
    assert_eq!(completed["params"]["auto"], true);
    assert_eq!(completed["params"]["auto_route"]["subagent_type"], "scout");
    assert_eq!(completed["params"]["metadata"]["subagent_type"], "scout");
    assert_eq!(completed["params"]["metadata"]["task_depth"], 1);
    assert!(
        completed["params"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("auto scout child answer"))
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(child_state["metadata"]["agent_profile"]["id"], "scout");
    assert_eq!(
        child_state["metadata"]["parent_tool_call_id"],
        "auto_task_scout"
    );
    let turn = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .ok_or("missing completed turn")?;
    assert_eq!(turn["params"]["source"], "auto_subagent");
    assert_eq!(turn["params"]["tool_calls"], 1);

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_executes_subagent_in_isolated_workspace() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-isolated-subagent")?;
    let session_root = temp.join("sessions");
    let agent_dir = temp.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(temp.join("parent.txt"), "parent\n")?;
    fs::write(
        agent_dir.join("isolated-writer.json"),
        serde_json::to_string_pretty(&json!({
            "id": "isolated-writer",
            "name": "Isolated Writer",
            "description": "Write-capable subagent that runs in an isolated workspace.",
            "mode": "subagent",
            "permission": "FULL",
            "prompt": "You write only inside your assigned workspace.",
            "tools": ["write"],
            "workspace_isolation": true,
            "max_steps": 3
        }))?,
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--permission",
            "FULL",
            "--format",
            "json",
            "@isolated-writer",
            "Write isolated.txt.",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_write_isolated","name":"write","input":{"file_path":"isolated.txt","content":"child\n"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "isolated child final")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/completed" && event["params"]["name"] == "task"
        })
        .ok_or_else(|| format!("missing isolated task completion: {events:?}"))?;
    assert_eq!(
        completed["params"]["metadata"]["workspace_isolation"]["enabled"],
        true
    );
    assert_eq!(
        completed["params"]["metadata"]["workspace_isolation"]["method"],
        "directory_copy"
    );
    let child_workspace = PathBuf::from(
        completed["params"]["metadata"]["workspace_isolation"]["workspace"]
            .as_str()
            .ok_or("missing isolated workspace")?,
    );
    assert_ne!(child_workspace, temp);
    assert!(!temp.join("isolated.txt").exists());
    assert_eq!(
        fs::read_to_string(child_workspace.join("isolated.txt"))?,
        "child\n"
    );
    let child_session_id = completed["params"]["metadata"]["session_id"]
        .as_str()
        .ok_or("missing child session id")?;
    let child_state: Value = serde_json::from_str(&fs::read_to_string(
        session_root
            .join(child_session_id)
            .join("state.latest.json"),
    )?)?;
    assert_eq!(
        child_state["workspace"],
        child_workspace.to_string_lossy().to_string()
    );
    assert_eq!(
        child_state["metadata"]["workspace_isolation"]["source_workspace"],
        completed["params"]["metadata"]["workspace_isolation"]["source_workspace"]
    );

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_discovers_and_executes_remote_mcp_tool() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-mcp-loop")?;
    let session_root = temp.join("sessions");
    let mcp_config = temp.join("mcp.json");
    let (port, server) = serve_mcp_json_rpc(2)?;
    fs::write(
        &mcp_config,
        format!(
            r#"{{
              "mcp": {{
                "demo": {{
                  "type": "remote",
                  "transport": "http",
                  "url": "http://127.0.0.1:{port}",
                  "enabled": true
                }}
              }}
            }}"#
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--mcp-config",
            path_str(&mcp_config),
            "--format",
            "json",
            "call",
            "mcp",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_mcp","name":"mcp_tool_demo_echo","input":{"text":"hi"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "mcp complete")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server
        .join()
        .expect("mcp server thread")
        .expect("mcp responses");
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .find(|event| event["method"] == "item/toolCall/completed")
        .ok_or("missing mcp tool completion")?;
    assert_eq!(completed["params"]["name"], "mcp_tool_demo_echo");
    assert_eq!(completed["params"]["output"], "MCP echo hi");
    assert_eq!(completed["params"]["metadata"]["backend"], "mcp");
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed" && event["params"]["final_answer"] == "mcp complete"
    }));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_discovers_and_executes_stdio_mcp_tool() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-stdio-mcp-loop")?;
    let session_root = temp.join("sessions");
    let mcp_config = temp.join("mcp.json");
    let server_script = temp.join("stdio_mcp_server.py");
    fs::write(&server_script, stdio_mcp_server_script())?;
    fs::write(
        &mcp_config,
        format!(
            r#"{{
              "mcpServers": {{
                "arbor-review": {{
                  "command": "python3",
                  "args": ["{}"],
                  "enabled": true,
                  "timeout_ms": 5000
                }}
              }}
            }}"#,
            server_script.display()
        ),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--mcp-config",
            path_str(&mcp_config),
            "--format",
            "json",
            "call",
            "mcp",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_mcp","name":"mcp_tool_arbor_review_arbor_review","input":{"text":"hi"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "stdio mcp complete")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .find(|event| event["method"] == "item/toolCall/completed")
        .ok_or("missing stdio mcp tool completion")?;
    assert_eq!(
        completed["params"]["name"],
        "mcp_tool_arbor_review_arbor_review"
    );
    assert_eq!(completed["params"]["output"], "stdio MCP echo hi");
    assert_eq!(completed["params"]["metadata"]["backend"], "mcp");
    assert_eq!(completed["params"]["metadata"]["mcp_transport"], "stdio");
    assert!(events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "stdio mcp complete"
    }));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_agent_profile_skill_config_public_and_not_provider_options() -> Result<(), Box<dyn Error>>
{
    let temp = temp_dir("openagent-cli-agent-skill-config")?;
    let agent_dir = temp.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("skillful.md"),
        r#"---
id: skillful
name: Skillful
description: Skill aware primary agent
mode: primary
permission:
  skill:
    alpha: deny
skills: ["alpha"]
skill_roots: ["shared-skills"]
skill_permissions:
  beta: allow
tools: ["read", "skill"]
model: gpt-skillful
options:
  reasoning_effort: medium
  skill_roots: ["must-not-leak"]
  skill_permissions:
    leaked: deny
---
You are the skillful profile.
"#,
    )?;

    let show = run_openagent(
        [
            "agent",
            "show",
            "skillful",
            "--workspace",
            path_str(&temp),
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let profile: Value = serde_json::from_slice(&show.stdout)?;
    assert_eq!(profile["skills"], json!(["alpha"]));
    assert_eq!(profile["skill_roots"], json!(["shared-skills"]));
    assert_eq!(profile["skill_permissions"][0]["pattern"], "alpha");
    assert_eq!(profile["skill_permissions"][0]["action"], "deny");
    assert_eq!(profile["skill_permissions"][1]["pattern"], "beta");
    assert_eq!(profile["skill_permissions"][1]["action"], "allow");
    assert_eq!(profile["model_options"]["reasoning_effort"], "medium");
    assert!(profile["model_options"].get("skill_roots").is_none());
    assert!(profile["model_options"].get("skill_permissions").is_none());

    let shared_skill = temp.join("shared-skills/rooted");
    fs::create_dir_all(&shared_skill)?;
    fs::write(
        shared_skill.join("SKILL.md"),
        r#"---
name: rooted
description: Rooted skill from profile roots
---
Use the rooted skill guidance.
"#,
    )?;
    let denied_skill = temp.join("shared-skills/alpha");
    fs::create_dir_all(&denied_skill)?;
    fs::write(
        denied_skill.join("SKILL.md"),
        r#"---
name: alpha
description: Alpha skill should be hidden by permission
---
Do not expose alpha guidance.
"#,
    )?;

    let (port, server, requests) = serve_http_capture_once_on_free_port(
        "application/json",
        json!({
            "id": "resp_skill_config",
            "output_text": "profile answer",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
        .to_string(),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&temp.join("sessions")),
            "--agent",
            "skillful",
            "--format",
            "json",
            "hello",
        ])
        .env_clear()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("OPENAI_WIRE_API", "responses")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server
        .join()
        .expect("provider server thread")
        .expect("provider response");
    let request = requests
        .lock()
        .expect("captured provider requests")
        .first()
        .cloned()
        .ok_or("missing provider request")?;
    assert!(request.contains("\"reasoning_effort\":\"medium\""));
    assert!(request.contains("<available_skills>"));
    assert!(request.contains("<name>rooted</name>"));
    assert!(request.contains("Rooted skill from profile roots"));
    assert!(!request.contains("<name>alpha</name>"));
    assert!(!request.contains("Alpha skill should be hidden by permission"));
    assert!(!request.contains("skill_roots"));
    assert!(!request.contains("skill_permissions"));
    assert!(!request.contains("must-not-leak"));

    let skill_run = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&temp.join("skill-sessions")),
            "--agent",
            "skillful",
            "--format",
            "json",
            "load rooted skill",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_skill_list","name":"skill","input":{"query":"root"}},{"call_id":"call_skill","name":"skill","input":{"name":"rooted"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "loaded rooted skill")
        .output()?;
    assert!(
        skill_run.status.success(),
        "{}",
        String::from_utf8_lossy(&skill_run.stderr)
    );
    let skill_events = String::from_utf8(skill_run.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(skill_events.iter().any(|event| {
        event["method"] == "item/toolCall/completed"
            && event["params"]["name"] == "skill"
            && event["params"]["output"]
                .as_str()
                .is_some_and(|output| output.contains("Use the rooted skill guidance."))
    }));
    let completed = skill_events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .ok_or("missing completed event")?;
    let session_id = completed["params"]["session_id"]
        .as_str()
        .ok_or("missing session id")?;
    let run_id = completed["params"]["run_id"]
        .as_str()
        .ok_or("missing run id")?;
    let session_events =
        read_session_event_records(&temp.join("skill-sessions"), session_id, run_id)?;
    assert!(session_events.iter().any(|event| {
        event["event"] == "skill.discovered"
            && event["kind"] == "skill"
            && event["attributes"]["query"] == "root"
            && event["attributes"]["skill_count"]
                .as_u64()
                .unwrap_or_default()
                >= 1
    }));
    assert!(session_events.iter().any(|event| {
        event["event"] == "skill.loaded"
            && event["kind"] == "skill"
            && event["attributes"]["skill_name"] == "rooted"
            && event["attributes"]["skill_dir"]
                .as_str()
                .is_some_and(|dir| dir.ends_with("shared-skills/rooted"))
            && event["attributes"]["skill_files"].as_array().is_some()
    }));

    let denied_skill_run = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&temp.join("denied-skill-sessions")),
            "--agent",
            "skillful",
            "--format",
            "json",
            "load alpha skill",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_skill_alpha","name":"skill","input":{"name":"alpha"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "alpha should not load")
        .output()?;
    assert!(
        denied_skill_run.status.success(),
        "{}",
        String::from_utf8_lossy(&denied_skill_run.stderr)
    );
    let denied_skill_events = String::from_utf8(denied_skill_run.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let failed_skill = denied_skill_events
        .iter()
        .find(|event| {
            event["method"] == "item/toolCall/failed" && event["params"]["name"] == "skill"
        })
        .ok_or("missing denied skill event")?;
    assert_eq!(
        failed_skill["params"]["metadata"]["permission_action"],
        "deny"
    );
    assert_eq!(
        failed_skill["params"]["metadata"]["error_kind"],
        "permission_denied"
    );

    fs::write(
        agent_dir.join("noskill.md"),
        r#"---
id: noskill
name: No Skill
description: Agent without skill tool
mode: primary
tools: ["read"]
skill_roots: ["shared-skills"]
---
You cannot load skills.
"#,
    )?;
    let (no_skill_port, no_skill_server, no_skill_requests) = serve_http_capture_once_on_free_port(
        "application/json",
        json!({
            "id": "resp_no_skill",
            "output_text": "no skill answer",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
        .to_string(),
    )?;
    let no_skill_output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&temp.join("no-skill-sessions")),
            "--agent",
            "noskill",
            "--format",
            "json",
            "hello without skills",
        ])
        .env_clear()
        .env("OPENAI_API_KEY", "test-key")
        .env(
            "OPENAI_BASE_URL",
            format!("http://127.0.0.1:{no_skill_port}"),
        )
        .env("OPENAI_WIRE_API", "responses")
        .output()?;
    assert!(
        no_skill_output.status.success(),
        "{}",
        String::from_utf8_lossy(&no_skill_output.stderr)
    );
    no_skill_server
        .join()
        .expect("no-skill provider server thread")
        .expect("no-skill provider response");
    let no_skill_request = no_skill_requests
        .lock()
        .expect("captured no-skill provider requests")
        .first()
        .cloned()
        .ok_or("missing no-skill provider request")?;
    assert!(!no_skill_request.contains("<available_skills>"));
    assert!(!no_skill_request.contains("Rooted skill from profile roots"));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_agent_system_prompt_refreshes_instructions_on_continue() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-dynamic-system-prompt")?;
    let session_root = temp.join("sessions");
    let agent_dir = temp.join(".openagent/agents");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("dynamic.md"),
        r#"---
id: dynamic
name: Dynamic
mode: primary
tools: ["read", "skill"]
model: gpt-dynamic
---
You are the dynamic profile.
"#,
    )?;
    fs::write(temp.join("OPENAGENT.md"), "FIRST_TURN_INSTRUCTION")?;
    let (port, server, requests) = serve_http_capture_responses_on_free_port(
        "application/json",
        vec![
            json!({
                "id": "resp_dynamic_first",
                "output_text": "first answer",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })
            .to_string(),
            json!({
                "id": "resp_dynamic_second",
                "output_text": "second answer",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })
            .to_string(),
        ],
    )?;

    let first = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--agent",
            "dynamic",
            "--format",
            "json",
            "first",
            "turn",
        ])
        .env_clear()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("OPENAI_WIRE_API", "responses")
        .output()?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    fs::write(temp.join("OPENAGENT.md"), "SECOND_TURN_INSTRUCTION")?;
    let second = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--continue",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--agent",
            "dynamic",
            "--format",
            "json",
            "second",
            "turn",
        ])
        .env_clear()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("OPENAI_WIRE_API", "responses")
        .output()?;
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    server
        .join()
        .expect("provider server thread")
        .expect("provider responses");
    let requests = requests.lock().expect("captured provider requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("FIRST_TURN_INSTRUCTION"));
    assert!(!requests[0].contains("SECOND_TURN_INSTRUCTION"));
    assert!(requests[1].contains("SECOND_TURN_INSTRUCTION"));
    assert!(!requests[1].contains("FIRST_TURN_INSTRUCTION"));
    assert!(requests[1].contains("You are the dynamic profile."));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_command_and_agent_profile_affect_real_run_state() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-command-agent")?;
    let command_dir = temp.join(".openagent/commands");
    fs::create_dir_all(&command_dir)?;
    fs::write(
        command_dir.join("summarize.md"),
        "Summarize this request: $ARGUMENTS",
    )?;
    let agent_create = run_openagent(
        [
            "agent",
            "create",
            "reviewer",
            "--workspace",
            path_str(&temp),
            "--provider",
            "openai",
            "--model",
            "openai/gpt-agent",
            "--permission",
            "READONLY",
            "--prompt",
            "You are a careful reviewer.",
            "--tool",
            "read",
            "--format",
            "json",
        ],
        None,
    )?;
    assert!(
        agent_create.status.success(),
        "{}",
        String::from_utf8_lossy(&agent_create.stderr)
    );
    let session_root = temp.join("sessions");
    let run = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--agent",
            "reviewer",
            "--command",
            "summarize",
            "--format",
            "json",
            "alpha",
            "beta",
        ])
        .env_clear()
        .env("OPENAGENT_MOCK_ANSWER", "profile complete")
        .output()?;
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let events = String::from_utf8(run.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        events[0]["params"]["prompt"],
        "Summarize this request: alpha beta"
    );
    let completed = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .ok_or("missing completion")?;
    let session_id = completed["params"]["session_id"]
        .as_str()
        .ok_or("missing session id")?;
    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(session_id).join("state.latest.json"),
    )?)?;
    assert_eq!(state["metadata"]["model"], "gpt-agent");
    assert_eq!(state["metadata"]["permission"], "READONLY");
    assert_eq!(state["metadata"]["agent_profile"]["id"], "reviewer");
    assert!(state["messages"].as_array().is_some_and(|messages| {
        !messages.iter().any(|message| {
            message["role"] == "system"
                && message["content"] == "You are a careful reviewer."
                && message["metadata"]["agent_profile"] == "reviewer"
        })
    }));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_queues_approval_for_dangerous_tool() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-agent-approval")?;
    let session_root = temp.join("sessions");
    let output = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
            "run",
            "a",
            "command",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_bash","name":"bash","input":{"command":"echo hi"}}]"#,
        )
        .output()?;
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        events
            .iter()
            .any(|event| event["method"] == "turn/approval_requested")
    );
    let approval = events
        .iter()
        .find(|event| event["method"] == "turn/approval_requested")
        .expect("approval event");
    assert_eq!(approval["params"]["approval"]["tool_name"], "bash");
    assert_eq!(
        approval["params"]["approval"]["reason"],
        "permission_required"
    );
    let completed = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .expect("failed completion event");
    assert_eq!(completed["params"]["status"], "paused");

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_approval_and_question_responses_resume_paused_runs() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-resume-queues")?;
    let session_root = temp.join("sessions");

    let approval_pause = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
            "run",
            "approval",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_bash","name":"bash","input":{"command":"printf approved"}}]"#,
        )
        .output()?;
    assert!(!approval_pause.status.success());
    let approval_events = String::from_utf8(approval_pause.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let approval = approval_events
        .iter()
        .find(|event| event["method"] == "turn/approval_requested")
        .ok_or("missing approval request")?;
    let approval_session = approval["params"]["session_id"]
        .as_str()
        .unwrap_or_default();
    let approval_response = run_openagent(
        [
            "approval",
            "respond",
            "--session-root",
            path_str(&session_root),
            "--session",
            approval_session,
            "--decision",
            "allow_once",
        ],
        None,
    )?;
    assert!(
        approval_response.status.success(),
        "{}",
        String::from_utf8_lossy(&approval_response.stderr)
    );
    let approval_resume = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--continue",
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
        ])
        .env_clear()
        .env("OPENAGENT_MOCK_ANSWER", "approval complete")
        .output()?;
    assert!(
        approval_resume.status.success(),
        "{}",
        String::from_utf8_lossy(&approval_resume.stderr)
    );
    let approval_resume_events = String::from_utf8(approval_resume.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(approval_resume_events.iter().any(|event| {
        event["method"] == "item/toolCall/completed" && event["params"]["output"] == "approved"
    }));
    assert!(approval_resume_events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "approval complete"
    }));

    let question_pause = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
            "ask",
            "question",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_question","name":"question","input":{"questions":[{"question":"Pick a mode","header":"Mode","options":[{"label":"Fast","description":"Use fast path"}]}]}}]"#,
        )
        .output()?;
    assert!(!question_pause.status.success());
    let question_events = String::from_utf8(question_pause.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let question = question_events
        .iter()
        .find(|event| event["method"] == "turn/question_requested")
        .ok_or("missing question request")?;
    let question_session = question["params"]["session_id"]
        .as_str()
        .unwrap_or_default();
    let question_response = run_openagent(
        [
            "question",
            "reply",
            "--session-root",
            path_str(&session_root),
            "--session",
            question_session,
            "--answer",
            "Fast",
        ],
        None,
    )?;
    assert!(
        question_response.status.success(),
        "{}",
        String::from_utf8_lossy(&question_response.stderr)
    );
    let question_resume = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--continue",
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
        ])
        .env_clear()
        .env("OPENAGENT_MOCK_ANSWER", "question complete")
        .output()?;
    assert!(
        question_resume.status.success(),
        "{}",
        String::from_utf8_lossy(&question_resume.stderr)
    );
    let question_resume_events = String::from_utf8(question_resume.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(question_resume_events.iter().any(|event| {
        event["method"] == "item/toolCall/completed"
            && event["params"]["output"]
                .as_str()
                .is_some_and(|text| text.contains("\"Pick a mode\"=\"Fast\""))
    }));
    assert!(question_resume_events.iter().any(|event| {
        event["method"] == "turn/completed"
            && event["params"]["final_answer"] == "question complete"
    }));

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_run_skip_permissions_auto_allows_ask_but_not_deny() -> Result<(), Box<dyn Error>> {
    let temp = temp_dir("openagent-cli-permission-skip")?;
    let session_root = temp.join("sessions");
    let allowed = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--dangerously-skip-permissions",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
            "run",
            "a",
            "command",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_bash","name":"bash","input":{"command":"printf allowed"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "done")
        .output()?;
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    let allowed_events = String::from_utf8(allowed.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        allowed_events
            .iter()
            .any(|event| event["method"] == "item/toolCall/completed"
                && event["params"]["output"] == "allowed")
    );

    let denied_path = temp.join("denied.txt");
    let denied = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--permission",
            "READONLY",
            "--dangerously-skip-permissions",
            "--workspace",
            path_str(&temp),
            "--session-root",
            path_str(&session_root),
            "--format",
            "json",
            "write",
            "a",
            "file",
        ])
        .env_clear()
        .env(
            "OPENAGENT_MOCK_TOOL_CALLS",
            r#"[{"call_id":"call_write","name":"write","input":{"file_path":"denied.txt","content":"nope"}}]"#,
        )
        .env("OPENAGENT_MOCK_ANSWER", "blocked")
        .output()?;
    assert!(
        denied.status.success(),
        "{}",
        String::from_utf8_lossy(&denied.stderr)
    );
    let denied_events = String::from_utf8(denied.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let failed = denied_events
        .iter()
        .find(|event| event["method"] == "item/toolCall/failed")
        .expect("denied tool failure event");
    assert_eq!(failed["params"]["metadata"]["permission_action"], "deny");
    assert_eq!(
        failed["params"]["metadata"]["error_kind"],
        "permission_denied"
    );
    assert!(!denied_path.exists());

    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_attach_and_tui_attach_use_remote_bridge_events() -> Result<(), Box<dyn Error>> {
    let port = free_port()?;
    let temp = temp_dir("openagent-cli-attach")?;
    let workspace = temp.join("workspace");
    let session_root = temp.join("sessions");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("context.txt"), "typed CLI attachment\n")?;
    let mut server = spawn_openagent_server(port, &workspace, &session_root)?;
    wait_for_attach(port)?;

    let url = format!("http://127.0.0.1:{port}");
    let run = run_openagent_vec(vec![
        "run".to_string(),
        "--attach".to_string(),
        url.clone(),
        "--server-token".to_string(),
        "secret".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--workspace".to_string(),
        path_str(&workspace).to_string(),
        "--file".to_string(),
        "context.txt".to_string(),
        "--model".to_string(),
        "gpt-5.5".to_string(),
        "--variant".to_string(),
        "high".to_string(),
        "--thinking".to_string(),
        "hello".to_string(),
        "attach".to_string(),
    ])?;
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let events = String::from_utf8(run.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        events
            .iter()
            .any(|event| event["method"] == "item/agentMessage/delta")
    );
    assert!(
        events
            .iter()
            .any(|event| event["method"] == "turn/completed")
    );
    let completed = events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .ok_or("missing attached completion")?;
    let session_id = completed["params"]["session_id"]
        .as_str()
        .ok_or("missing attached session id")?;
    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(session_id).join("state.latest.json"),
    )?)?;
    let user = state["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .ok_or("missing attached user message")?;
    assert_eq!(user["content"], "hello attach");
    assert_eq!(
        user["metadata"]["context_attachments"][0]["content"],
        "typed CLI attachment\n"
    );
    assert_eq!(
        state["metadata"]["context_pack"]["receipt"]["item_kind_counts"]["attachment_file"],
        1
    );
    assert_eq!(state["metadata"]["model"], "gpt-5.5");
    assert_eq!(state["metadata"]["variant"], "high");
    assert_eq!(state["metadata"]["thinking"], "high");

    let client = run_openagent_vec(vec![
        "client".to_string(),
        "--server-url".to_string(),
        url.clone(),
        "--server-token".to_string(),
        "secret".to_string(),
        "--workspace".to_string(),
        path_str(&workspace).to_string(),
        "--session".to_string(),
        session_id.to_string(),
        "--file".to_string(),
        "context.txt".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--model".to_string(),
        "gpt-5.4".to_string(),
        "--variant".to_string(),
        "medium".to_string(),
        "client".to_string(),
        "context".to_string(),
    ])?;
    assert!(
        client.status.success(),
        "{}",
        String::from_utf8_lossy(&client.stderr)
    );
    let state: Value = serde_json::from_str(&fs::read_to_string(
        session_root.join(session_id).join("state.latest.json"),
    )?)?;
    let user = state["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|message| message["role"] == "user")
        })
        .ok_or("missing client user message")?;
    assert_eq!(user["content"], "client context");
    assert_eq!(
        user["metadata"]["context_attachments"][0]["content"],
        "typed CLI attachment\n"
    );
    assert_eq!(state["metadata"]["model"], "gpt-5.4");
    assert_eq!(state["metadata"]["variant"], "medium");

    let attach = run_openagent_vec(vec![
        "attach".to_string(),
        url.clone(),
        "--server-token".to_string(),
        "secret".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ])?;
    assert!(attach.status.success());
    let payload: Value = serde_json::from_slice(&attach.stdout)?;
    assert_eq!(payload["attached"], true);
    assert!(
        payload["sessions"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let tui_attach = run_openagent_vec(vec![
        "tui".to_string(),
        "--attach".to_string(),
        url,
        "--server-token".to_string(),
        "secret".to_string(),
        "--format".to_string(),
        "json".to_string(),
    ])?;
    assert!(tui_attach.status.success());
    let payload: Value = serde_json::from_slice(&tui_attach.stdout)?;
    assert_eq!(payload["attached"], true);

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

#[test]
fn binary_local_and_bridge_context_pack_receipts_have_surface_parity() -> Result<(), Box<dyn Error>>
{
    let port = free_port()?;
    let temp = temp_dir("openagent-cli-context-parity")?;
    let workspace = temp.join("workspace");
    let local_session_root = temp.join("local-sessions");
    let bridge_session_root = temp.join("bridge-sessions");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("context.txt"), "surface parity attachment\n")?;
    let provider_response = json!({
        "id": "resp_context_parity",
        "output_text": "surface parity answer",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
    .to_string();
    let (provider_port, provider_server, provider_requests) =
        serve_http_capture_responses_on_free_port(
            "application/json",
            vec![provider_response.clone(), provider_response],
        )?;
    let provider_url = format!("http://127.0.0.1:{provider_port}");

    let local = Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "run",
            "--skip-doctor",
            "--workspace",
            path_str(&workspace),
            "--session-root",
            path_str(&local_session_root),
            "--file",
            "context.txt",
            "--model",
            "gpt-5.5",
            "--format",
            "json",
            "surface",
            "parity",
        ])
        .env_clear()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", &provider_url)
        .env("OPENAI_WIRE_API", "responses")
        .output()?;
    assert!(
        local.status.success(),
        "{}",
        String::from_utf8_lossy(&local.stderr)
    );
    let local_events = String::from_utf8(local.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let local_session_id = local_events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .and_then(|event| event["params"]["session_id"].as_str())
        .ok_or("missing local parity session")?;
    let local_state: Value = serde_json::from_str(&fs::read_to_string(
        local_session_root
            .join(local_session_id)
            .join("state.latest.json"),
    )?)?;

    let mut server = spawn_openagent_server_with_provider(
        port,
        &workspace,
        &bridge_session_root,
        &provider_url,
    )?;
    wait_for_attach(port)?;
    let url = format!("http://127.0.0.1:{port}");
    let remote = run_openagent_vec(vec![
        "run".to_string(),
        "--attach".to_string(),
        url,
        "--server-token".to_string(),
        "secret".to_string(),
        "--workspace".to_string(),
        path_str(&workspace).to_string(),
        "--file".to_string(),
        "context.txt".to_string(),
        "--model".to_string(),
        "gpt-5.5".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "surface".to_string(),
        "parity".to_string(),
    ])?;
    assert!(
        remote.status.success(),
        "{}",
        String::from_utf8_lossy(&remote.stderr)
    );
    let remote_events = String::from_utf8(remote.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let remote_session_id = remote_events
        .iter()
        .find(|event| event["method"] == "turn/completed")
        .and_then(|event| event["params"]["session_id"].as_str())
        .ok_or("missing remote parity session")?;
    let remote_state: Value = serde_json::from_str(&fs::read_to_string(
        bridge_session_root
            .join(remote_session_id)
            .join("state.latest.json"),
    )?)?;

    let local_receipt = &local_state["metadata"]["context_pack"]["receipt"];
    let remote_receipt = &remote_state["metadata"]["context_pack"]["receipt"];
    for field in [
        "message_role_counts",
        "tool_manifest_count",
        "tool_names",
        "model_option_keys",
        "item_kind_counts",
        "item_delivery_counts",
    ] {
        assert_eq!(
            local_receipt[field], remote_receipt[field],
            "context receipt field `{field}` differs across local CLI and Bridge"
        );
    }
    assert_eq!(
        local_state["messages"][0]["content"],
        remote_state["messages"][0]["content"]
    );
    assert_eq!(
        local_state["messages"][0]["metadata"]["context_attachments"][0]["content"],
        remote_state["messages"][0]["metadata"]["context_attachments"][0]["content"]
    );
    provider_server
        .join()
        .expect("context parity provider server")
        .expect("context parity provider responses");
    let provider_requests = provider_requests
        .lock()
        .expect("captured context parity provider requests");
    assert_eq!(provider_requests.len(), 2);
    let local_provider_request: Value = serde_json::from_str(&provider_requests[0])?;
    let remote_provider_request: Value = serde_json::from_str(&provider_requests[1])?;
    assert_eq!(
        local_provider_request["input"],
        remote_provider_request["input"]
    );
    assert_eq!(
        local_provider_request["tools"],
        remote_provider_request["tools"]
    );
    assert_eq!(
        local_receipt["provider_input_hash"],
        remote_receipt["provider_input_hash"]
    );

    let _ = server.kill();
    let _ = fs::remove_dir_all(temp);
    Ok(())
}

fn read_fixture() -> Result<Value, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/golden/rust_rewrite/cli_commands.json");
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn run_openagent<const N: usize>(
    args: [&str; N],
    cwd: Option<&Path>,
) -> Result<std::process::Output, Box<dyn Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openagent"));
    command.args(args).env_clear();
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    Ok(command.output()?)
}

fn run_openagent_vec(args: Vec<String>) -> Result<std::process::Output, Box<dyn Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args(args)
        .env_clear()
        .output()?)
}

fn spawn_openagent_server(
    port: u16,
    workspace: &Path,
    session_root: &Path,
) -> Result<Child, Box<dyn Error>> {
    let port = port.to_string();
    Ok(Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            &port,
            "--workspace",
            path_str(workspace),
            "--session-root",
            path_str(session_root),
            "--auth-token",
            "secret",
        ])
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn spawn_openagent_server_with_provider(
    port: u16,
    workspace: &Path,
    session_root: &Path,
    provider_url: &str,
) -> Result<Child, Box<dyn Error>> {
    let port = port.to_string();
    Ok(Command::new(env!("CARGO_BIN_EXE_openagent"))
        .args([
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            &port,
            "--workspace",
            path_str(workspace),
            "--session-root",
            path_str(session_root),
            "--auth-token",
            "secret",
        ])
        .env_clear()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", provider_url)
        .env("OPENAI_WIRE_API", "responses")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn wait_for_attach(port: u16) -> Result<(), Box<dyn Error>> {
    let url = format!("http://127.0.0.1:{port}");
    for _ in 0..50 {
        let output = run_openagent_vec(vec![
            "attach".to_string(),
            url.clone(),
            "--server-token".to_string(),
            "secret".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ])?;
        if output.status.success() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("server did not accept attach".into())
}

fn free_port() -> Result<u16, Box<dyn Error>> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn serve_http_once_on_free_port(
    content_type: &str,
    body: String,
) -> Result<(u16, MockServer), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    Ok((
        port,
        serve_http_once_with_listener(listener, content_type, body),
    ))
}

fn serve_http_once_with_listener(
    listener: TcpListener,
    content_type: &str,
    body: String,
) -> MockServer {
    let content_type = content_type.to_string();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
        let _ = read_http_request_body(&mut stream)?;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .map_err(|error| error.to_string())
    })
}

fn serve_http_responses_on_free_port(
    responses: Vec<String>,
) -> Result<(u16, MockServer), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let server = thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let _ = read_http_request_body(&mut stream)?;
            stream
                .write_all(response.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    });
    Ok((port, server))
}

fn serve_http_capture_once_on_free_port(
    content_type: &str,
    body: String,
) -> Result<(u16, MockServer, CapturedRequests), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let content_type = content_type.to_string();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
        let request = read_http_request_body(&mut stream)?;
        captured
            .lock()
            .map_err(|error| error.to_string())?
            .push(request);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .map_err(|error| error.to_string())
    });
    Ok((port, server, requests))
}

fn serve_http_capture_responses_on_free_port(
    content_type: &str,
    bodies: Vec<String>,
) -> Result<(u16, MockServer, CapturedRequests), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let content_type = content_type.to_string();
    let server = thread::spawn(move || {
        for body in bodies {
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let request = read_http_request_body(&mut stream)?;
            captured
                .lock()
                .map_err(|error| error.to_string())?
                .push(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    });
    Ok((port, server, requests))
}

fn serve_dripping_sse_provider() -> Result<(u16, MockServer), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
        let _ = read_http_request_body(&mut stream)?;
        let first =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello \"},\"finish_reason\":null}]}\n\n";
        let second =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"streamed\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n";
        let done = b"data: [DONE]\n\n";
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .map_err(|error| error.to_string())?;
        write_http_chunk(&mut stream, first)?;
        stream.flush().map_err(|error| error.to_string())?;
        thread::sleep(Duration::from_millis(500));
        write_http_chunk(&mut stream, second)?;
        write_http_chunk(&mut stream, done)?;
        stream
            .write_all(b"0\r\n\r\n")
            .map_err(|error| error.to_string())?;
        stream.flush().map_err(|error| error.to_string())
    });
    Ok((port, server))
}

fn write_http_chunk(stream: &mut std::net::TcpStream, chunk: &[u8]) -> Result<(), String> {
    stream
        .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
        .map_err(|error| error.to_string())?;
    stream.write_all(chunk).map_err(|error| error.to_string())?;
    stream.write_all(b"\r\n").map_err(|error| error.to_string())
}

fn serve_mcp_json_rpc(expected_requests: usize) -> Result<(u16, MockServer), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let server = thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            let body = read_http_request_body(&mut stream)?;
            let request: Value = serde_json::from_str(&body).map_err(|error| error.to_string())?;
            let method = request.get("method").and_then(Value::as_str).unwrap_or("");
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let response = if method == "tools/list" {
                json_response_body(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "title": "Echo",
                            "description": "Echo text",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"text": {"type": "string"}},
                                "required": ["text"]
                            }
                        }]
                    }
                }))
            } else if method == "tools/call" {
                let text = request
                    .get("params")
                    .and_then(|params| params.get("arguments"))
                    .and_then(|arguments| arguments.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json_response_body(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": format!("MCP echo {text}")}],
                        "isError": false
                    }
                }))
            } else {
                json_response_body(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "method not found"}
                }))
            };
            stream
                .write_all(response.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    });
    Ok((port, server))
}

fn stdio_mcp_server_script() -> &'static str {
    r#"import json
import sys


def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode("utf-8").strip()
        if not line:
            break
        key, _, value = line.partition(":")
        headers[key.lower()] = value.strip()
    length = int(headers["content-length"])
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))


def write_message(value):
    raw = json.dumps(value).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(raw))
    sys.stdout.buffer.write(raw)
    sys.stdout.buffer.flush()


while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({
            "jsonrpc": "2.0",
            "id": message.get("id"),
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "stdio-test", "version": "0.0.0"},
            },
        })
    elif method == "tools/list":
        write_message({
            "jsonrpc": "2.0",
            "id": message.get("id"),
            "result": {
                "tools": [{
                    "name": "arbor_review",
                    "title": "Arbor Review",
                    "description": "Review text",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                    },
                }],
            },
        })
    elif method == "tools/call":
        text = message.get("params", {}).get("arguments", {}).get("text", "")
        write_message({
            "jsonrpc": "2.0",
            "id": message.get("id"),
            "result": {
                "content": [{"type": "text", "text": "stdio MCP echo " + text}],
                "isError": False,
            },
        })
    elif method == "shutdown":
        write_message({"jsonrpc": "2.0", "id": message.get("id"), "result": {}})
    elif method == "exit":
        break
"#
}

fn read_http_request_body(stream: &mut std::net::TcpStream) -> Result<String, String> {
    let mut buffer = [0_u8; 8192];
    let read = stream
        .read(&mut buffer)
        .map_err(|error| error.to_string())?;
    let raw = String::from_utf8_lossy(&buffer[..read]).to_string();
    let (headers, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "invalid HTTP request".to_string())?;
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(body.len());
    let mut body = body.as_bytes().to_vec();
    while body.len() < content_length {
        let read = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    body.truncate(content_length);
    String::from_utf8(body).map_err(|error| error.to_string())
}

fn json_response_body(value: Value) -> String {
    let body = value.to_string();
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn python3_executable() -> Result<Option<String>, Box<dyn Error>> {
    let output = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8(output.stdout)?.trim().to_string();
    Ok((!path.is_empty()).then_some(path))
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

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or("")
}

fn read_session_event_records(
    session_root: &Path,
    session_id: &str,
    run_id: &str,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let path = session_root
        .join(session_id)
        .join("runs")
        .join(run_id)
        .join("events.jsonl");
    let raw = fs::read_to_string(path)?;
    raw.lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

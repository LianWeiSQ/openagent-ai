use super::*;

pub fn core_crate_name() -> &'static str {
    openagent_core::crate_name()
}

#[must_use]
pub fn parse_cli_args(argv: &[&str]) -> Value {
    if argv.is_empty() {
        return json!({
            "command": null,
            "base_url": null,
            "model": null,
            "wire_api": null,
            "max_steps": null,
            "workspace": null,
            "skip_doctor": false,
        });
    }
    match argv[0] {
        "doctor" => json!({
            "command": "doctor",
            "format": value_after(argv, "--format").unwrap_or("text"),
            "base_url": value_after(argv, "--base-url"),
            "model": value_after(argv, "--model"),
        }),
        "run" => json!({
            "command": "run",
            "workspace": value_after(argv, "--workspace").or_else(|| value_after(argv, "--dir")),
            "skip_doctor": argv.contains(&"--skip-doctor"),
            "format": value_after(argv, "--format").unwrap_or("text"),
            "message": positional_after_options(argv, &["run"]),
        }),
        "mcp" if argv.get(1) == Some(&"add") => json!({
            "command": "mcp",
            "mcp_command": "add",
            "name": argv.get(2).copied().unwrap_or_default(),
            "url": value_after(argv, "--url").unwrap_or_default(),
            "transport": value_after(argv, "--transport").unwrap_or("auto"),
            "timeout_ms": value_after(argv, "--timeout-ms").and_then(|item| item.parse::<u64>().ok()).unwrap_or(30_000),
            "format": value_after(argv, "--format").unwrap_or("table"),
        }),
        command => json!({"command": command}),
    }
}

#[must_use]
pub fn model_env_fixture() -> Value {
    json!({
        "default": {
            "OPENAI_BASE_URL": DEFAULT_BASE_URL,
            "OPENAI_MODEL": DEFAULT_MODEL,
            "OPENAI_WIRE_API": DEFAULT_WIRE_API,
            "OPENAGENT_APP_MAX_STEPS": DEFAULT_MAX_STEPS,
        },
        "override": {
            "OPENAI_BASE_URL": "http://127.0.0.1:9999",
            "OPENAI_MODEL": "gpt-test",
            "OPENAI_WIRE_API": "chat",
            "OPENAGENT_APP_MAX_STEPS": "8",
        },
    })
}

#[must_use]
pub fn doctor_text_ok_result() -> CliRunResult {
    CliRunResult {
        exit_code: 0,
        stdout: [
            "OpenAgent doctor",
            "- provider: openai (OpenAI)",
            "- OPENAI_BASE_URL: http://gateway.test",
            "- OPENAI_MODEL: gpt-test",
            "- OPENAI_WIRE_API: chat",
            "- OPENAI_API_KEY: missing",
            "- model endpoint: ok (http://gateway.test/v1/models)",
            "",
        ]
        .join("\n"),
        stderr: String::new(),
    }
}

#[must_use]
pub fn doctor_json_failed_payload() -> Value {
    json!({
        "provider": "openai",
        "provider_label": "OpenAI",
        "base_url": "http://gateway.test",
        "model": "gpt-test",
        "wire_api": "responses",
        "api_key_env": "OPENAI_API_KEY",
        "api_key_set": true,
        "native": false,
        "healthy": false,
        "dependency_checked": false,
        "dependency_ok": true,
        "dependency_message": null,
        "model_endpoint_checked": true,
        "model_endpoint_ok": false,
        "model_endpoint_message": "connection refused",
    })
}

#[must_use]
pub fn doctor_json_failed_result() -> CliRunResult {
    let payload = doctor_json_failed_payload();
    CliRunResult {
        exit_code: 2,
        stdout: format!("{}\n", stable_json_dumps(&payload)),
        stderr: String::new(),
    }
}

#[must_use]
pub fn doctor_anthropic_payload() -> Value {
    json!({
        "provider": "anthropic",
        "provider_label": "Anthropic",
        "base_url": null,
        "model": "claude-test",
        "wire_api": "messages",
        "api_key_env": "ANTHROPIC_API_KEY",
        "api_key_set": true,
        "native": true,
        "healthy": true,
        "dependency_checked": true,
        "dependency_ok": true,
        "dependency_message": "optional dependency 'anthropic' is installed",
        "model_endpoint_checked": false,
        "model_endpoint_ok": true,
        "model_endpoint_message": "skipped OpenAI-compatible /models probe for native provider",
    })
}

#[must_use]
pub fn auth_login_payload() -> Value {
    json!({
        "status": "logged_in",
        "provider": "groq",
        "auth_file": format!("{GOAL10_ROOT}/auth.json"),
        "record": public_provider_record(
            "groq",
            "groq-secret",
            "https://api.groq.example/v1",
            "llama-fixture",
            Value::Null,
            "auth_file",
            Some(1_781_842_000_123u64),
        ),
    })
}

#[must_use]
pub fn auth_list_payload() -> Value {
    json!({
        "auth_file": format!("{GOAL10_ROOT}/auth.json"),
        "providers": [
            public_provider_record(
                "groq",
                "groq-secret",
                "https://api.groq.example/v1",
                "llama-fixture",
                Value::Null,
                "auth_file",
                Some(1_781_842_000_123u64),
            )
        ],
    })
}

#[must_use]
pub fn auth_methods_payload() -> Value {
    let methods =
        provider_auth_methods("openrouter", &BTreeSet::new()).expect("openrouter methods build");
    json!({"provider": "openrouter", "methods": methods})
}

#[must_use]
pub fn custom_command_list_payload() -> Value {
    json!({
        "commands": [custom_command_record(false)],
    })
}

#[must_use]
pub fn custom_command_show_payload() -> Value {
    custom_command_record(true)
}

#[must_use]
pub fn rendered_custom_command_prompt() -> String {
    format!(
        "Review notes.txt with all args: notes.txt carefully.\n\n\
         Attached file: {GOAL10_WORKSPACE}/notes.txt\n\n\
         ```text\nAlpha note\nBeta note\n\n```"
    )
}

#[must_use]
pub fn custom_command_render_text_result() -> CliRunResult {
    CliRunResult {
        exit_code: 0,
        stdout: format!("{}\n", rendered_custom_command_prompt()),
        stderr: String::new(),
    }
}

#[must_use]
pub fn custom_command_render_json_payload() -> Value {
    json!({
        "command": custom_command_record(false),
        "prompt": rendered_custom_command_prompt(),
    })
}

#[must_use]
pub fn config_init_payload() -> Value {
    json!({
        "created": true,
        "path": format!("{GOAL10_WORKSPACE}/.openagent/openagent.env"),
        "workspace": GOAL10_WORKSPACE,
        "api_key_written": true,
        "server_token_written": false,
        "mode": "0o600",
        "next": ["openagent doctor", "openagent"],
    })
}

#[must_use]
pub fn config_show_payload() -> Value {
    json!({
        "workspace": GOAL10_WORKSPACE,
        "env_file": format!("{GOAL10_WORKSPACE}/.openagent/openagent.env"),
        "auth_file": format!("{GOAL10_ROOT}/auth.json"),
        "session_root": format!("{GOAL10_WORKSPACE}/.openagent/sessions"),
        "openai": {
            "base_url": "http://config.test/v1",
            "model": "gpt-config",
            "wire_api": "responses",
            "api_key": "set",
            "max_steps": "12",
        },
        "app_bridge": {
            "server_url": DEFAULT_SERVER_URL,
            "server_token": "set",
            "server_token_env": DEFAULT_SERVER_TOKEN_ENV,
        },
    })
}

#[must_use]
pub fn mcp_add_payload() -> Value {
    json!({
        "config_path": format!("{GOAL10_ROOT}/mcp.json"),
        "server": {
            "name": "demo",
            "url": "https://[redacted]@example.com/mcp?token=[redacted]&safe=1",
            "transport": "http",
            "enabled": true,
            "timeout_ms": 45_000,
            "header_names": ["Authorization", "X-Team"],
            "headers": {"Authorization": "[redacted]", "X-Team": "[redacted]"},
        },
        "updated": true,
    })
}

#[must_use]
pub fn mcp_list_table_result() -> CliRunResult {
    CliRunResult {
        exit_code: 0,
        stdout: [
            "name  enabled  transport  timeout_ms  headers               url",
            "----  -------  ---------  ----------  --------------------  ----------------------------------------------------------",
            "demo  True     http       45000       Authorization,X-Team  https://[redacted]@example.com/mcp?token=[redacted]&safe=1",
            "",
        ]
        .join("\n"),
        stderr: String::new(),
    }
}

#[must_use]
pub fn mcp_doctor_payload() -> Value {
    json!({
        "config_path": format!("{GOAL10_ROOT}/mcp.json"),
        "configured": true,
        "enabled": true,
        "server_count": 1,
        "ok": true,
        "refresh_error": null,
        "servers": [{
            "name": "demo",
            "url": "https://[redacted]@example.com/mcp?token=[redacted]&safe=1",
            "enabled": true,
            "configured_transport": "http",
            "selected_transport": null,
            "status": "idle",
            "tool_count": 0,
            "last_error": null,
            "last_refreshed_at": null,
            "tools": [],
            "ok": true,
        }],
    })
}

#[must_use]
pub fn skills_list_payload() -> Value {
    json!({
        "skills": [skill_info_fixture()],
        "query": "root",
        "loaded_count": 1,
        "scanned_files": 1,
        "invalid_count": 0,
        "duplicate_count": 0,
    })
}

#[must_use]
pub fn skills_show_payload() -> Value {
    json!({
        "name": "rooted",
        "description": "Rooted fixture skill",
        "location": format!("{GOAL10_WORKSPACE}/.openagent/skills/rooted/SKILL.md"),
        "directory": format!("{GOAL10_WORKSPACE}/.openagent/skills/rooted"),
        "metadata": {"audience": "fixture"},
        "content": "Use rooted fixture guidance.",
        "rendered": "# Skill: rooted\n\nUse rooted fixture guidance.",
    })
}

#[must_use]
pub fn skills_doctor_payload() -> Value {
    json!({
        "loaded_count": 1,
        "scanned_files": 1,
        "invalid_count": 0,
        "duplicate_count": 0,
        "skills": [skill_info_fixture()],
        "issues": [],
    })
}

fn skill_info_fixture() -> Value {
    json!({
        "name": "rooted",
        "description": "Rooted fixture skill",
        "location": format!("{GOAL10_WORKSPACE}/.openagent/skills/rooted/SKILL.md"),
        "directory": format!("{GOAL10_WORKSPACE}/.openagent/skills/rooted"),
        "metadata": {"audience": "fixture"},
        "score": 13,
    })
}

#[must_use]
pub fn cli_commands_fixture() -> Value {
    json!({
        "schema_version": 1,
        "parser": {
            "default": {
                "argv": [],
                "namespace": parse_cli_args(&[]),
            },
            "doctor_json": {
                "argv": ["doctor", "--format", "json"],
                "namespace": parse_cli_args(&["doctor", "--format", "json"]),
            },
            "run_json": {
                "argv": ["run", "--workspace", GOAL10_WORKSPACE, "--skip-doctor", "--format", "json", "hello", "world"],
                "namespace": parse_cli_args(&["run", "--workspace", GOAL10_WORKSPACE, "--skip-doctor", "--format", "json", "hello", "world"]),
            },
            "mcp_add": {
                "argv": ["mcp", "add", "demo", "--config", format!("{GOAL10_ROOT}/mcp.json"), "--url", "https://example.com/mcp"],
                "namespace": parse_cli_args(&["mcp", "add", "demo", "--config", &format!("{GOAL10_ROOT}/mcp.json"), "--url", "https://example.com/mcp"]),
            },
        },
        "model_env": model_env_fixture(),
        "doctor": {
            "text_ok": run_result_json_without_stderr(&doctor_text_ok_result(), None),
            "json_failed": run_result_json_without_stderr(&doctor_json_failed_result(), Some(doctor_json_failed_payload())),
            "anthropic_json": {
                "exit_code": 0,
                "json": doctor_anthropic_payload(),
                "stdout": format!("{}\n", stable_json_dumps(&doctor_anthropic_payload())),
                "openai_probe_called": false,
            },
        },
        "auth": {
            "login": run_result_json(&CliRunResult::ok_json(&auth_login_payload()), Some(auth_login_payload())),
            "list": run_result_json(&CliRunResult::ok_json(&auth_list_payload()), Some(auth_list_payload())),
            "methods": run_result_json(&CliRunResult::ok_json(&auth_methods_payload()), Some(auth_methods_payload())),
        },
        "custom_commands": {
            "list": run_result_json(&CliRunResult::ok_json(&custom_command_list_payload()), Some(custom_command_list_payload())),
            "show": run_result_json(&CliRunResult::ok_json(&custom_command_show_payload()), Some(custom_command_show_payload())),
            "render_text": run_result_json(&custom_command_render_text_result(), None),
            "render_json": run_result_json(&CliRunResult::ok_json(&custom_command_render_json_payload()), Some(custom_command_render_json_payload())),
        },
        "config": {
            "init": run_result_json(&CliRunResult::ok_json(&config_init_payload()), Some(config_init_payload())),
            "show": run_result_json(&CliRunResult::ok_json(&config_show_payload()), Some(config_show_payload())),
        },
        "mcp_cli": {
            "add": run_result_json(&CliRunResult::ok_json(&mcp_add_payload()), Some(mcp_add_payload())),
            "list_table": run_result_json(&mcp_list_table_result(), None),
            "doctor": run_result_json(&CliRunResult::ok_json(&mcp_doctor_payload()), Some(mcp_doctor_payload())),
        },
        "skills_cli": {
            "list": run_result_json(&CliRunResult::ok_json(&skills_list_payload()), Some(skills_list_payload())),
            "show": run_result_json(&CliRunResult::ok_json(&skills_show_payload()), Some(skills_show_payload())),
            "doctor": run_result_json(&CliRunResult::ok_json(&skills_doctor_payload()), Some(skills_doctor_payload())),
        },
    })
}

fn run_result_json(result: &CliRunResult, json_value: Option<Value>) -> Value {
    let mut object = Map::from_iter([
        ("exit_code".to_string(), json!(result.exit_code)),
        ("stdout".to_string(), json!(result.stdout)),
        ("stderr".to_string(), json!(result.stderr)),
    ]);
    if let Some(json_value) = json_value {
        object.insert("json".to_string(), json_value);
    }
    Value::Object(object)
}

fn run_result_json_without_stderr(result: &CliRunResult, json_value: Option<Value>) -> Value {
    let mut object = Map::from_iter([
        ("exit_code".to_string(), json!(result.exit_code)),
        ("stdout".to_string(), json!(result.stdout)),
    ]);
    if let Some(json_value) = json_value {
        object.insert("json".to_string(), json_value);
    }
    Value::Object(object)
}

fn public_provider_record(
    provider: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
    wire_api: Value,
    source: &str,
    updated_at_ms: Option<u64>,
) -> Value {
    let env = default_env_mapping(provider).expect("provider env mapping exists");
    let auth_methods = provider_auth_methods(provider, &BTreeSet::new())
        .expect("provider auth methods build")
        .into_iter()
        .map(|mut method| {
            if let Some(object) = method.as_object_mut() {
                let keep = ["id", "type", "env_api_key", "implemented", "status"];
                let api_key_env = object
                    .get("env")
                    .and_then(Value::as_object)
                    .and_then(|env| env.get("api_key"))
                    .cloned()
                    .unwrap_or(Value::Null);
                object.retain(|key, _| keep.contains(&key.as_str()));
                object.insert("env_api_key".to_string(), api_key_env);
            }
            method
        })
        .collect::<Vec<_>>();
    let env_status = env
        .iter()
        .map(|(field, name)| (field.clone(), json!({"name": name, "status": "missing"})))
        .collect::<Map<_, _>>();
    json!({
        "provider": provider,
        "type": "api",
        "source": source,
        "api_key": mask_secret(api_key),
        "has_api_key": true,
        "base_url": base_url,
        "model": model,
        "wire_api": wire_api,
        "env": env,
        "env_status": env_status,
        "auth_methods": auth_methods,
        "methods": ["api_key"],
        "updated_at_ms": updated_at_ms,
    })
}

fn custom_command_record(include_template: bool) -> Value {
    let mut object = Map::from_iter([
        ("name".to_string(), json!("review")),
        (
            "path".to_string(),
            json!(format!("{GOAL10_WORKSPACE}/.openagent/commands/review.md")),
        ),
        ("scope".to_string(), json!("project")),
        ("description".to_string(), json!("Review a target file.")),
        ("agent".to_string(), json!("reviewer")),
        ("model".to_string(), json!("gpt-command")),
    ]);
    if include_template {
        object.insert(
            "template".to_string(),
            json!("Review $1 with all args: $ARGUMENTS.\n\n@notes.txt"),
        );
    }
    Value::Object(object)
}

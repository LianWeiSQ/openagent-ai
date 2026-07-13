use super::prompt::discover_mcp_server_tools;
use super::*;

pub(super) fn mcp_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(mcp_help());
    }
    if !mcp_remote_requested(args) && args[0] == "test" && mcp_local_config_requested(&args[1..]) {
        return mcp_test(&args[1..]);
    }
    if mcp_remote_requested(args) && mcp_local_config_requested(&args[1..]) {
        return err_text(2, mcp_remote_ignores_local_config_message(&args[0]));
    }
    if mcp_lifecycle_command(&args[0]) && mcp_local_config_requested(&args[1..]) {
        return err_text(2, mcp_lifecycle_requires_bridge_message(&args[0]));
    }
    if mcp_remote_requested(args) || mcp_remote_only_command(&args[0]) {
        return mcp_remote_command(args);
    }
    match args[0].as_str() {
        "add" => mcp_add(&args[1..]),
        "list" | "ls" => mcp_list(&args[1..]),
        "show" => mcp_show(&args[1..]),
        "remove" | "rm" => mcp_remove(&args[1..]),
        "auth" => mcp_auth(&args[1..]),
        "logout" => mcp_logout(&args[1..]),
        "doctor" => mcp_doctor(&args[1..]),
        "debug" => mcp_debug(&args[1..]),
        _ => err_text(2, format!("unknown mcp command: {}", args[0])),
    }
}

fn mcp_remote_requested(args: &[String]) -> bool {
    value_for(args, &["--server-url", "--attach"]).is_some()
}

fn mcp_local_config_requested(args: &[String]) -> bool {
    value_for(args, &["--mcp-config", "--config", "--workspace", "--dir"]).is_some()
}

fn mcp_lifecycle_command(command: &str) -> bool {
    matches!(command, "start" | "stop" | "restart" | "enable" | "disable")
}

fn mcp_remote_only_command(command: &str) -> bool {
    matches!(
        command,
        "test" | "start" | "stop" | "restart" | "enable" | "disable"
    )
}

fn mcp_lifecycle_requires_bridge_message(command: &str) -> String {
    format!(
        "mcp {command} uses the Bridge lifecycle registry and cannot run directly from --mcp-config/--config.\n\
         Start the Rust Bridge API service with the desired workspace/config, then run:\n\
         openagent mcp {command} <server> --server-url <url> --server-token <token>\n\
         For one-shot local connectivity, use: openagent mcp test <server> --mcp-config <file>"
    )
}

fn mcp_remote_ignores_local_config_message(command: &str) -> String {
    format!(
        "mcp {command} with --server-url/--attach uses the Bridge server workspace/config; local --mcp-config/--config/--workspace/--dir cannot be applied client-side.\n\
         Start the Rust Bridge API service with the desired workspace/config, then retry without the local config flag."
    )
}

fn mcp_remote_command(args: &[String]) -> CliRunResult {
    let command = args[0].as_str();
    let rest = &args[1..];
    let server_url = value_for(rest, &["--server-url", "--attach"])
        .or_else(|| value_for(args, &["--server-url", "--attach"]))
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
    let client = remote::bridge_client(&server_url, &remote_auth_from_args(rest));
    let refresh = has_flag(rest, &["--refresh", "--check"]) || command == "doctor";
    let result = match command {
        "list" | "ls" | "doctor" => client.mcp_status(refresh),
        "show" | "debug" => {
            let Some(name) = mcp_remote_server_name(rest, command) else {
                return err_text(2, format!("mcp {command} requires a server name"));
            };
            client
                .mcp_status(refresh)
                .map(|payload| mcp_remote_show_payload(&server_url, &name, payload))
        }
        "test" => {
            let Some(name) = mcp_remote_server_name(rest, command) else {
                return err_text(2, "mcp test requires a server name");
            };
            client.mcp_server_test(&name)
        }
        "start" | "stop" | "restart" => {
            let Some(name) = mcp_remote_server_name(rest, command) else {
                return err_text(2, format!("mcp {command} requires a server name"));
            };
            client.mcp_server_lifecycle(&name, command)
        }
        "enable" | "disable" => {
            let Some(name) = mcp_remote_server_name(rest, command) else {
                return err_text(2, format!("mcp {command} requires a server name"));
            };
            client.mcp_server_update(&name, json!({"enabled": command == "enable"}))
        }
        other => {
            return err_text(
                2,
                format!("mcp {other} does not support --server-url/--attach yet"),
            );
        }
    };
    let payload = match result {
        Ok(payload) => mcp_remote_payload(&server_url, payload),
        Err(error) => return err_text(1, error),
    };
    if value_for(rest, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else if command == "show" || command == "debug" {
        mcp_remote_show_text(&payload)
    } else {
        ok_text(mcp_remote_status_text(&payload))
    }
}

fn mcp_remote_server_name(args: &[String], command: &str) -> Option<String> {
    positional_args(args, &mcp_remote_value_flags())
        .into_iter()
        .find(|value| value != command)
}

fn mcp_remote_value_flags() -> Vec<&'static str> {
    vec![
        "--server-url",
        "--attach",
        "--server-token",
        "--server-token-env",
        "--username",
        "-u",
        "--password",
        "-p",
        "--format",
        "--mcp-config",
        "--config",
        "--workspace",
        "--dir",
    ]
}

fn mcp_remote_payload(server_url: &str, payload: Value) -> Value {
    match payload {
        Value::Object(mut object) => {
            object.insert("remote".to_string(), json!(true));
            object.insert("server_url".to_string(), json!(server_url));
            Value::Object(object)
        }
        other => json!({"remote": true, "server_url": server_url, "payload": other}),
    }
}

fn mcp_remote_show_payload(server_url: &str, name: &str, payload: Value) -> Value {
    let server = payload
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server["name"].as_str() == Some(name))
                .cloned()
        });
    json!({
        "remote": true,
        "server_url": server_url,
        "name": name,
        "server": server,
        "found": server.is_some(),
        "mcp": payload,
    })
}

fn mcp_remote_show_text(payload: &Value) -> CliRunResult {
    let Some(server) = payload.get("server").filter(|value| !value.is_null()) else {
        return err_text(
            1,
            format!(
                "MCP server not found: {}",
                payload["name"].as_str().unwrap_or_default()
            ),
        );
    };
    ok_text(format!(
        "{} {} {} tools={} lifecycle={} pid={}",
        server["name"].as_str().unwrap_or("mcp"),
        server["status"].as_str().unwrap_or("-"),
        server["selected_transport"]
            .as_str()
            .or_else(|| server["transport"].as_str())
            .unwrap_or("-"),
        server["tool_count"].as_u64().unwrap_or(0),
        server["lifecycle_status"].as_str().unwrap_or("-"),
        server["lifecycle_pid"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    ))
}

fn mcp_remote_status_text(payload: &Value) -> String {
    let servers = payload
        .get("servers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if servers.is_empty() {
        return "No MCP servers configured".to_string();
    }
    let rows = servers
        .iter()
        .map(|server| {
            vec![
                server["name"].as_str().unwrap_or("").to_string(),
                if server["enabled"].as_bool().unwrap_or(false) {
                    "yes".to_string()
                } else {
                    "no".to_string()
                },
                server["status"].as_str().unwrap_or("-").to_string(),
                server["selected_transport"]
                    .as_str()
                    .or_else(|| server["transport"].as_str())
                    .unwrap_or("-")
                    .to_string(),
                server["tool_count"]
                    .as_u64()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                server["lifecycle_status"]
                    .as_str()
                    .unwrap_or("-")
                    .to_string(),
                server["lifecycle_pid"]
                    .as_u64()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                mcp_remote_endpoint_label(server),
            ]
        })
        .collect::<Vec<_>>();
    format!(
        "Remote MCP Servers ({})\n{}",
        payload["server_url"].as_str().unwrap_or(DEFAULT_SERVER_URL),
        render_table(
            &[
                "Name",
                "Enabled",
                "Status",
                "Transport",
                "Tools",
                "Lifecycle",
                "PID",
                "Endpoint",
            ],
            &rows,
        )
    )
}

fn mcp_remote_endpoint_label(server: &Value) -> String {
    if let Some(command) = server["command"].as_str().filter(|value| !value.is_empty()) {
        let args_count = server["args_count"].as_u64().unwrap_or_default();
        if args_count == 0 {
            command.to_string()
        } else {
            format!("{command} +{args_count}")
        }
    } else if server["remote_url_configured"].as_bool().unwrap_or(false) {
        "remote URL".to_string()
    } else {
        "-".to_string()
    }
}

fn mcp_add(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--url",
            "--command",
            "--arg",
            "--env",
            "--cwd",
            "--transport",
            "--header",
            "--timeout-ms",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp add requires a server name");
    };
    let url = value_for(args, &["--url"]);
    let command = value_for(args, &["--command"]);
    if url.is_none() && command.is_none() {
        return err_text(2, "mcp add requires --url or --command");
    }
    let config_path = mcp_config_path(args);
    let mut config = read_json_file(&config_path);
    let servers = ensure_object_field(&mut config, "mcp");
    let headers = parse_headers(&values_for(args, &["--header"]));
    let server = if let Some(command) = command {
        let mut command_parts = vec![command];
        command_parts.extend(values_for(args, &["--arg"]));
        let mut server = json!({
            "type": "local",
            "command": command_parts,
            "transport": "stdio",
            "enabled": !has_flag(args, &["--disabled"]),
            "timeout_ms": value_for(args, &["--timeout-ms"]).and_then(|value| value.parse::<u64>().ok()).unwrap_or(30_000),
            "environment": parse_headers(&values_for(args, &["--env"])),
        });
        if let Some(cwd) = value_for(args, &["--cwd"])
            && let Some(object) = server.as_object_mut()
        {
            object.insert("cwd".to_string(), json!(cwd));
        }
        server
    } else {
        json!({
            "type": "remote",
            "url": url.unwrap_or_default(),
            "transport": value_for(args, &["--transport"]).unwrap_or_else(|| "auto".to_string()),
            "enabled": !has_flag(args, &["--disabled"]),
            "timeout_ms": value_for(args, &["--timeout-ms"]).and_then(|value| value.parse::<u64>().ok()).unwrap_or(30_000),
            "headers": headers,
        })
    };
    servers.insert(name.clone(), server);
    let public_server = mcp_public_server(name, servers.get(name).unwrap_or(&Value::Null));
    if let Err(error) = write_json_file(&config_path, &config) {
        return err_text(1, error);
    }
    let payload = json!({"config_path": config_path.to_string_lossy(), "server": public_server, "updated": true});
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else {
        ok_text(format!("updated MCP server {name}"))
    }
}

fn mcp_list(args: &[String]) -> CliRunResult {
    let config_path = mcp_config_path(args);
    let servers = mcp_public_servers(&read_json_file(&config_path));
    let payload = json!({"config_path": config_path.to_string_lossy(), "servers": servers});
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else if payload["servers"].as_array().is_none_or(Vec::is_empty) {
        ok_text("No MCP servers configured")
    } else {
        let rows = payload["servers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|server| {
                let headers = server["header_names"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",");
                vec![
                    server["name"].as_str().unwrap_or("").to_string(),
                    if server["enabled"].as_bool().unwrap_or(false) {
                        "yes".to_string()
                    } else {
                        "no".to_string()
                    },
                    server["transport"].as_str().unwrap_or("auto").to_string(),
                    server["timeout_ms"]
                        .as_u64()
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    if headers.is_empty() {
                        "-".to_string()
                    } else {
                        headers
                    },
                    server["endpoint"].as_str().unwrap_or("").to_string(),
                ]
            })
            .collect::<Vec<_>>();
        ok_text(format!(
            "MCP Servers\n{}",
            render_table(
                &[
                    "Name",
                    "Enabled",
                    "Transport",
                    "Timeout",
                    "Headers",
                    "Endpoint"
                ],
                &rows
            )
        ))
    }
}

fn mcp_show(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp show requires a server name");
    };
    let config_path = mcp_config_path(args);
    let config = read_json_file(&config_path);
    let server = mcp_server_from_config(&config, name);
    let Some(server) = server else {
        return err_text(1, format!("MCP server not found: {name}"));
    };
    let payload = json!({"config_path": config_path.to_string_lossy(), "server": mcp_public_server(name, server)});
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else {
        ok_text(format!(
            "{} {}",
            name,
            payload["server"]["endpoint"].as_str().unwrap_or("")
        ))
    }
}

fn mcp_test(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp test requires a server name");
    };
    let config_path = mcp_config_path(args);
    let config = if config_path.exists() {
        match load_mcp_config(&config_path.to_string_lossy()) {
            Ok(config) => config,
            Err(error) => return err_text(1, error),
        }
    } else {
        return err_text(
            1,
            format!("MCP config file not found: {}", config_path.display()),
        );
    };
    let Some(server) = config
        .servers
        .iter()
        .find(|server| server.name == *name)
        .cloned()
    else {
        return err_text(1, format!("MCP server not found: {name}"));
    };
    let workspace = workspace_from_args(args);
    let mut manager = RemoteMcpManager::new(config);
    let tested_at = Some(now_ms_cli() as f64 / 1000.0);
    let result = match discover_mcp_server_tools(&server, &workspace) {
        Ok((transport, tools)) => {
            let descriptors = build_tool_descriptors_from_values(&server, &tools);
            let tool_count = descriptors.len();
            let _ = manager.set_server_tools(
                &server.name,
                Some(transport),
                "connected",
                tested_at,
                descriptors,
            );
            Ok(tool_count)
        }
        Err(error) => {
            let _ = manager.set_server_error(&server.name, "failed", error.clone(), tested_at);
            Err(error)
        }
    };
    let snapshot = serde_json::to_value(manager.snapshot()).unwrap_or_else(|_| json!({}));
    let server_payload = snapshot
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server["name"].as_str() == Some(name))
                .cloned()
        })
        .unwrap_or_else(|| json!({"name": name, "status": "unknown"}));
    let payload = json!({
        "config_path": config_path.to_string_lossy(),
        "workspace": workspace.to_string_lossy(),
        "name": name,
        "ok": result.is_ok(),
        "server": server_payload,
        "error": result.as_ref().err(),
    });
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        return if result.is_ok() {
            CliRunResult::ok_json(&payload)
        } else {
            CliRunResult {
                exit_code: 1,
                stdout: format!("{}\n", stable_json_dumps(&payload)),
                stderr: String::new(),
            }
        };
    }
    match result {
        Ok(tool_count) => ok_text(format!("MCP server {name} connected: {tool_count} tool(s)")),
        Err(error) => err_text(1, format!("MCP server {name} failed: {error}")),
    }
}

fn mcp_remove(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp remove requires a server name");
    };
    let config_path = mcp_config_path(args);
    let mut config = read_json_file(&config_path);
    let removed = config
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.remove(name))
        .is_some();
    if let Err(error) = write_json_file(&config_path, &config) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(
        &json!({"config_path": config_path.to_string_lossy(), "name": name, "removed": removed}),
    )
}

fn mcp_auth(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(
            "Usage: openagent mcp auth <list|status|login|set-token|callback> [options]",
        );
    }
    match args[0].as_str() {
        "list" | "ls" | "status" => mcp_doctor(&args[1..]),
        "login" | "start" => mcp_auth_login(&args[1..]),
        "callback" => mcp_auth_callback(&args[1..]),
        "set-token" => {
            let positionals = positional_args(
                &args[1..],
                &[
                    "--mcp-config",
                    "--config",
                    "--workspace",
                    "--dir",
                    "--bearer-token",
                    "--header-name",
                    "--format",
                ],
            );
            let Some(name) = positionals.first() else {
                return err_text(2, "mcp auth set-token requires a server name");
            };
            let Some(token) = value_for(&args[1..], &["--bearer-token"]) else {
                return err_text(
                    2,
                    "mcp auth set-token requires --bearer-token in this Rust CLI path",
                );
            };
            let header = value_for(&args[1..], &["--header-name"])
                .unwrap_or_else(|| "Authorization".to_string());
            let config_path = mcp_config_path(&args[1..]);
            let mut config = read_json_file(&config_path);
            let Some(server) = config
                .get_mut("mcp")
                .and_then(Value::as_object_mut)
                .and_then(|servers| servers.get_mut(name))
                .and_then(Value::as_object_mut)
            else {
                return err_text(1, format!("MCP server not found: {name}"));
            };
            let headers = server.entry("headers").or_insert_with(|| json!({}));
            if let Some(headers) = headers.as_object_mut() {
                headers.insert(header.clone(), json!(format!("Bearer {token}")));
            }
            if let Err(error) = write_json_file(&config_path, &config) {
                return err_text(1, error);
            }
            CliRunResult::ok_json(
                &json!({"config_path": config_path.to_string_lossy(), "name": name, "header": header, "updated": true}),
            )
        }
        _ => err_text(2, format!("unknown mcp auth command: {}", args[0])),
    }
}

fn mcp_auth_login(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--client-id",
            "--client-secret",
            "--authorize-url",
            "--token-url",
            "--redirect-uri",
            "--scope",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp auth login requires a server name");
    };
    let config_path = mcp_config_path(args);
    let mut config = read_json_file(&config_path);
    let Some(server) = config
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.get_mut(name))
        .and_then(Value::as_object_mut)
    else {
        return err_text(1, format!("MCP server not found: {name}"));
    };
    let state = new_cli_id("mcp_oauth");
    let redirect_uri = value_for(args, &["--redirect-uri"])
        .unwrap_or_else(|| "http://127.0.0.1:8787/mcp/oauth/callback".to_string());
    let authorize_url = value_for(args, &["--authorize-url"])
        .or_else(|| {
            server
                .get("authorize_url")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            let url = server
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("{}/authorize", url.trim_end_matches('/'))
        });
    let client_id =
        value_for(args, &["--client-id"]).unwrap_or_else(|| "openagent-cli".to_string());
    let scope = value_for(args, &["--scope"]).unwrap_or_else(|| "mcp".to_string());
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        authorize_url,
        url_encode(&client_id),
        url_encode(&redirect_uri),
        url_encode(&scope),
        url_encode(&state)
    );
    server.insert(
        "oauth".to_string(),
        json!({
            "state": state,
            "client_id": client_id,
            "client_secret": value_for(args, &["--client-secret"]).unwrap_or_default(),
            "authorize_url": authorize_url,
            "token_url": value_for(args, &["--token-url"]),
            "redirect_uri": redirect_uri,
            "scope": scope,
            "status": "authorization_required",
            "updated_at_ms": now_ms_cli(),
        }),
    );
    if let Err(error) = write_json_file(&config_path, &config) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(&json!({
        "config_path": config_path.to_string_lossy(),
        "name": name,
        "status": "authorization_required",
        "authorize_url": url,
    }))
}

fn mcp_auth_callback(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--code",
            "--state",
            "--access-token",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp auth callback requires a server name");
    };
    let config_path = mcp_config_path(args);
    let mut config = read_json_file(&config_path);
    let Some(server) = config
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.get_mut(name))
        .and_then(Value::as_object_mut)
    else {
        return err_text(1, format!("MCP server not found: {name}"));
    };
    let expected_state = server
        .get("oauth")
        .and_then(|value| value.get("state"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(state) = value_for(args, &["--state"])
        && !expected_state.is_empty()
        && state != expected_state
    {
        return err_text(1, "MCP OAuth state mismatch");
    }
    let access_token = value_for(args, &["--access-token"]).unwrap_or_else(|| {
        value_for(args, &["--code"])
            .map(|code| format!("code:{code}"))
            .unwrap_or_default()
    });
    if access_token.is_empty() {
        return err_text(2, "mcp auth callback requires --code or --access-token");
    }
    let headers = server.entry("headers").or_insert_with(|| json!({}));
    if let Some(headers) = headers.as_object_mut() {
        headers.insert(
            "Authorization".to_string(),
            json!(format!("Bearer {access_token}")),
        );
    }
    server.insert(
        "oauth".to_string(),
        json!({"status": "authorized", "updated_at_ms": now_ms_cli()}),
    );
    if let Err(error) = write_json_file(&config_path, &config) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(
        &json!({"config_path": config_path.to_string_lossy(), "name": name, "status": "authorized"}),
    )
}

fn mcp_logout(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp logout requires a server name");
    };
    let config_path = mcp_config_path(args);
    let mut config = read_json_file(&config_path);
    let removed = config
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.get_mut(name))
        .and_then(Value::as_object_mut)
        .and_then(|server| server.get_mut("headers"))
        .and_then(Value::as_object_mut)
        .and_then(|headers| headers.remove("Authorization"))
        .is_some();
    if let Err(error) = write_json_file(&config_path, &config) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(
        &json!({"config_path": config_path.to_string_lossy(), "name": name, "removed": removed}),
    )
}

fn mcp_doctor(args: &[String]) -> CliRunResult {
    let config_path = mcp_config_path(args);
    let config = if config_path.exists() {
        match load_mcp_config(&config_path.to_string_lossy()) {
            Ok(config) => config,
            Err(error) => return err_text(1, error),
        }
    } else {
        openagent_mcp::McpConfig::default()
    };
    let refresh = has_flag(args, &["--refresh"]);
    let workspace = workspace_from_args(args);
    let mut manager = RemoteMcpManager::new(config.clone());
    let mut refresh_error = None::<String>;
    if refresh {
        for server in config.servers.iter().filter(|server| server.enabled) {
            match discover_mcp_server_tools(server, &workspace) {
                Ok((transport, tools)) => {
                    let descriptors = build_tool_descriptors_from_values(server, &tools);
                    let _ = manager.set_server_tools(
                        &server.name,
                        Some(transport),
                        "connected",
                        Some(now_ms_cli() as f64 / 1000.0),
                        descriptors,
                    );
                }
                Err(error) => {
                    refresh_error = Some(error);
                }
            }
        }
    }
    let snapshot = serde_json::to_value(manager.snapshot()).unwrap_or_else(|_| json!({}));
    let servers = snapshot
        .get("servers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let payload = json!({
        "config_path": config_path.to_string_lossy(),
        "configured": !servers.is_empty(),
        "enabled": servers.iter().any(|server| server["enabled"].as_bool().unwrap_or(false)),
        "server_count": servers.len(),
        "ok": refresh_error.is_none() && servers.iter().all(|server| server["status"].as_str() != Some("failed")),
        "refresh_error": refresh_error,
        "servers": servers,
    });
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else {
        ok_text(format!(
            "{} MCP server(s)",
            payload["server_count"].as_u64().unwrap_or(0)
        ))
    }
}

fn mcp_debug(args: &[String]) -> CliRunResult {
    let positionals = positional_args(
        args,
        &[
            "--mcp-config",
            "--config",
            "--workspace",
            "--dir",
            "--format",
        ],
    );
    let Some(name) = positionals.first() else {
        return err_text(2, "mcp debug requires a server name");
    };
    let config_path = mcp_config_path(args);
    let config = read_json_file(&config_path);
    let server = mcp_server_from_config(&config, name);
    let Some(server) = server else {
        return err_text(1, format!("MCP server not found: {name}"));
    };
    CliRunResult::ok_json(&json!({"server": mcp_public_server(name, server)}))
}

fn mcp_server_from_config<'a>(config: &'a Value, name: &str) -> Option<&'a Value> {
    config
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|servers| servers.get(name))
        .or_else(|| {
            config
                .get("mcp")
                .and_then(|mcp| mcp.get("servers"))
                .and_then(Value::as_object)
                .and_then(|servers| servers.get(name))
        })
        .or_else(|| {
            config
                .get("mcp")
                .and_then(Value::as_object)
                .and_then(|servers| servers.get(name))
        })
}

use super::*;

pub(super) fn config_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(config_help());
    }
    match args[0].as_str() {
        "init" => config_init(&args[1..]),
        "show" => config_show(&args[1..]),
        _ => err_text(2, format!("unknown config command: {}", args[0])),
    }
}

fn config_init(args: &[String]) -> CliRunResult {
    let workspace = workspace_from_args(args);
    let path = value_for(args, &["--path"])
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join(".openagent/openagent.env"));
    if path.exists() && !has_flag(args, &["--force"]) {
        return err_text(1, format!("config file already exists: {}", path.display()));
    }
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return err_text(1, format!("failed to create config directory: {error}"));
    }
    let server_token =
        has_flag(args, &["--with-server-token"]).then(|| "generated-local-token".to_string());
    let lines = [
        value_for(args, &["--api-key"]).map(|value| format!("OPENAI_API_KEY={value}")),
        Some(format!(
            "OPENAI_BASE_URL={}",
            value_for(args, &["--base-url"]).unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
        )),
        Some(format!(
            "OPENAI_MODEL={}",
            value_for(args, &["--model"]).unwrap_or_else(|| DEFAULT_MODEL.to_string())
        )),
        Some(format!(
            "OPENAI_WIRE_API={}",
            value_for(args, &["--wire-api"]).unwrap_or_else(|| DEFAULT_WIRE_API.to_string())
        )),
        Some(format!(
            "OPENAGENT_BRIDGE_MAX_STEPS={}",
            value_for(args, &["--max-steps"]).unwrap_or_else(|| DEFAULT_MAX_STEPS.to_string())
        )),
        server_token.map(|token| format!("{DEFAULT_SERVER_TOKEN_ENV}={token}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    if let Err(error) = fs::write(&path, format!("{lines}\n")) {
        return err_text(1, format!("failed to write config: {error}"));
    }
    chmod_private(&path);
    let payload = json!({
        "created": true,
        "path": path.to_string_lossy(),
        "workspace": workspace.to_string_lossy(),
        "api_key_written": value_for(args, &["--api-key"]).is_some(),
        "server_token_written": has_flag(args, &["--with-server-token"]),
        "mode": "0o600",
        "next": ["openagent doctor", "openagent"],
    });
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else {
        ok_text(format!("created {}", path.display()))
    }
}

fn config_show(args: &[String]) -> CliRunResult {
    let workspace = workspace_from_args(args);
    let env_file = workspace.join(".openagent/openagent.env");
    let session_root = value_for(args, &["--session-root"])
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join(".openagent/sessions"));
    let openai = resolve_provider_config("openai", args).ok();
    let payload = json!({
        "workspace": workspace.to_string_lossy(),
        "env_file": env_file.to_string_lossy(),
        "auth_file": auth_file_from_args(args).to_string_lossy(),
        "session_root": session_root.to_string_lossy(),
        "openai": {
            "base_url": openai.as_ref().map(|config| config.base_url.as_str()).unwrap_or(DEFAULT_BASE_URL),
            "base_url_source": openai.as_ref().map(|config| config.base_url_source.as_str()).unwrap_or("default"),
            "model": openai.as_ref().map(|config| config.model.as_str()).unwrap_or(DEFAULT_MODEL),
            "model_source": openai.as_ref().map(|config| config.model_source.as_str()).unwrap_or("default"),
            "wire_api": openai.as_ref().map(|config| config.wire_api.as_str()).unwrap_or(DEFAULT_WIRE_API),
            "wire_api_source": openai.as_ref().map(|config| config.wire_api_source.as_str()).unwrap_or("default"),
            "api_key": if openai.as_ref().is_some_and(|config| config.api_key.is_some()) {"set"} else {"missing"},
            "api_key_source": openai.as_ref().and_then(|config| config.api_key_source.as_deref()),
            "max_steps": env::var("OPENAGENT_BRIDGE_MAX_STEPS").unwrap_or_else(|_| DEFAULT_MAX_STEPS.to_string()),
        },
        "bridge": {
            "server_url": value_for(args, &["--server-url"]).unwrap_or_else(|| DEFAULT_SERVER_URL.to_string()),
            "server_token": if env::var(DEFAULT_SERVER_TOKEN_ENV).is_ok_and(|value| !value.is_empty()) {"set"} else {"missing"},
            "server_token_env": DEFAULT_SERVER_TOKEN_ENV,
        },
    });
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else {
        ok_text(format!(
            "workspace: {}\nenv_file: {}\nsession_root: {}",
            workspace.display(),
            env_file.display(),
            session_root.display()
        ))
    }
}

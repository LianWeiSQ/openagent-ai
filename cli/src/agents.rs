use super::prompt::{
    agent_profile_public_value, available_agent_profiles, load_agent_profile_by_name,
    validate_agents_from_args,
};
use super::*;

pub(super) fn agent_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(
            "Usage: openagent agent <list|create|show|delete|run> [name] [--agents <json-or-path>] [--model <id>] [--provider <id>] [--mode <primary|subagent|all>] [--permission <ruleset>] [--prompt <text>] [--tool <name>]",
        );
    }
    if let Err(error) = validate_agents_from_args(args) {
        return err_text(2, error);
    }
    match args[0].as_str() {
        "list" | "ls" => {
            let agents = available_agent_profiles(args)
                .iter()
                .map(agent_profile_public_value)
                .collect::<Vec<_>>();
            CliRunResult::ok_json(&json!({"agents": agents}))
        }
        "create" => {
            let positionals = positional_args(
                args,
                &[
                    "--workspace",
                    "--agents",
                    "--dir",
                    "--model",
                    "-m",
                    "--provider",
                    "--mode",
                    "--permission",
                    "--description",
                    "--prompt",
                    "--tool",
                    "--format",
                ],
            );
            let Some(name) = positionals.get(1).or_else(|| positionals.first()) else {
                return err_text(2, "agent create requires a name");
            };
            let agent_id = sanitize_identifier(name);
            let mode = value_for(args, &["--mode"]).unwrap_or_else(|| "primary".to_string());
            if !matches!(mode.as_str(), "primary" | "subagent" | "all") {
                return err_text(2, "agent mode must be primary, subagent, or all");
            }
            let dir = agent_registry_dir(args);
            let path = dir.join(format!("{agent_id}.json"));
            let payload = json!({
                "schema_version": "openagent.agent.v1",
                "id": agent_id,
                "name": name,
                "model": value_for(args, &["--model", "-m"]),
                "provider": value_for(args, &["--provider"]),
                "mode": mode,
                "permission": value_for(args, &["--permission"]).unwrap_or_else(|| "ask".to_string()),
                "description": value_for(args, &["--description"]),
                "prompt": value_for(args, &["--prompt"]),
                "tools": values_for(args, &["--tool"]),
                "updated_at_ms": now_ms_cli(),
            });
            if let Err(error) = write_json_file(&path, &payload) {
                return err_text(1, error);
            }
            CliRunResult::ok_json(
                &json!({"created": true, "path": path.to_string_lossy(), "agent": payload}),
            )
        }
        "show" => {
            let positionals =
                positional_args(args, &["--workspace", "--dir", "--agents", "--format"]);
            let Some(name) = positionals.get(1).or_else(|| positionals.first()) else {
                return err_text(2, "agent show requires a name");
            };
            match load_agent_profile_by_name(args, name) {
                Ok(profile) => CliRunResult::ok_json(&agent_profile_public_value(&profile)),
                Err(error) => err_text(1, error),
            }
        }
        "delete" | "rm" => {
            let positionals =
                positional_args(args, &["--workspace", "--dir", "--agents", "--format"]);
            let Some(name) = positionals.get(1).or_else(|| positionals.first()) else {
                return err_text(2, "agent delete requires a name");
            };
            let path = agent_registry_dir(args).join(format!("{}.json", sanitize_identifier(name)));
            let removed = fs::remove_file(&path).is_ok();
            CliRunResult::ok_json(&json!({"removed": removed, "path": path.to_string_lossy()}))
        }
        "run" => {
            let positionals = positional_args(
                args,
                &[
                    "--workspace",
                    "--agents",
                    "--dir",
                    "--session-root",
                    "--format",
                    "--model",
                    "-m",
                    "--provider",
                    "--api-key",
                    "--base-url",
                    "--wire-api",
                    "--mcp-config",
                ],
            );
            let Some(name) = positionals.get(1).or_else(|| positionals.first()) else {
                return err_text(2, "agent run requires an agent name");
            };
            let prompt = positionals
                .iter()
                .skip(if positionals.get(1).is_some() { 2 } else { 1 })
                .cloned()
                .collect::<Vec<_>>();
            if prompt.is_empty() {
                return err_text(2, "agent run requires a prompt");
            }
            let mut run_args = Vec::new();
            copy_cli_options(
                args,
                &[
                    "--workspace",
                    "--agents",
                    "--dir",
                    "--session-root",
                    "--format",
                    "--model",
                    "-m",
                    "--provider",
                    "--api-key",
                    "--base-url",
                    "--wire-api",
                    "--skip-doctor",
                    "--stream",
                    "--mcp-config",
                ],
                &mut run_args,
            );
            run_args.push("--agent".to_string());
            run_args.push(sanitize_identifier(name));
            run_args.extend(prompt);
            run_prompt_command(&run_args)
        }
        other => err_text(2, format!("unknown agent command: {other}")),
    }
}

pub(super) fn plugin_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(
            "Usage: openagent plugin <install|list|show|enable|disable|remove|run> [module-or-path] [--global] [--force]",
        );
    }
    match args[0].as_str() {
        "install" | "add" => plugin_install(&args[1..]),
        "list" | "ls" => plugin_list(&args[1..]),
        "show" => plugin_show(&args[1..]),
        "enable" => plugin_set_enabled(&args[1..], true),
        "disable" => plugin_set_enabled(&args[1..], false),
        "remove" | "rm" | "uninstall" => plugin_remove(&args[1..]),
        "run" => plugin_run(&args[1..]),
        _ => plugin_install(args),
    }
}

fn plugin_install(args: &[String]) -> CliRunResult {
    let module = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .cloned()
        .unwrap_or_default();
    if module.is_empty() {
        return err_text(2, "plugin install requires a module or path");
    }
    let path = if has_flag(args, &["--global", "-g"]) {
        home_dir().join(".config/openagent/plugins.json")
    } else {
        workspace_from_args(args).join(".openagent/plugins.json")
    };
    let mut config = read_json_file(&path);
    let plugins = ensure_object_field(&mut config, "plugins");
    let manifest = plugin_manifest_from_source(&module);
    let plugin_id = manifest
        .get("id")
        .or_else(|| manifest.get("name"))
        .and_then(Value::as_str)
        .map(sanitize_identifier)
        .unwrap_or_else(|| sanitize_identifier(&module));
    if plugins.contains_key(&plugin_id) && !has_flag(args, &["--force", "-f"]) {
        return err_text(1, format!("plugin already registered: {plugin_id}"));
    }
    plugins.insert(
        plugin_id.clone(),
        json!({
            "schema_version": "openagent.plugin_install.v1",
            "id": plugin_id,
            "module": module,
            "source": plugin_source_kind(&module),
            "enabled": true,
            "manifest": manifest,
            "updated_at_ms": now_ms_cli(),
        }),
    );
    if let Err(error) = write_json_file(&path, &config) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(
        &json!({"installed": true, "path": path.to_string_lossy(), "plugin_id": plugin_id}),
    )
}

fn plugin_list(args: &[String]) -> CliRunResult {
    let path = plugin_registry_path(args);
    let config = read_json_file(&path);
    let plugins = config
        .get("plugins")
        .and_then(Value::as_object)
        .map(|items| items.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    CliRunResult::ok_json(&json!({"path": path.to_string_lossy(), "plugins": plugins}))
}

fn plugin_show(args: &[String]) -> CliRunResult {
    let positionals = positional_args(args, &["--workspace", "--dir", "--format"]);
    let Some(id) = positionals.first() else {
        return err_text(2, "plugin show requires an id");
    };
    let path = plugin_registry_path(args);
    let plugin_id = sanitize_identifier(id);
    let plugin = read_json_file(&path)
        .get("plugins")
        .and_then(|plugins| plugins.get(&plugin_id))
        .cloned()
        .unwrap_or(Value::Null);
    CliRunResult::ok_json(&json!({"path": path.to_string_lossy(), "plugin": plugin}))
}

fn plugin_set_enabled(args: &[String], enabled: bool) -> CliRunResult {
    let positionals = positional_args(args, &["--workspace", "--dir", "--format"]);
    let Some(id) = positionals.first() else {
        return err_text(2, "plugin enable/disable requires an id");
    };
    let path = plugin_registry_path(args);
    let mut config = read_json_file(&path);
    let plugins = ensure_object_field(&mut config, "plugins");
    let plugin_id = sanitize_identifier(id);
    let Some(plugin) = plugins.get_mut(&plugin_id).and_then(Value::as_object_mut) else {
        return err_text(1, format!("plugin not installed: {plugin_id}"));
    };
    plugin.insert("enabled".to_string(), json!(enabled));
    plugin.insert("updated_at_ms".to_string(), json!(now_ms_cli()));
    if let Err(error) = write_json_file(&path, &config) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(&json!({"plugin_id": plugin_id, "enabled": enabled}))
}

fn plugin_remove(args: &[String]) -> CliRunResult {
    let positionals = positional_args(args, &["--workspace", "--dir", "--format"]);
    let Some(id) = positionals.first() else {
        return err_text(2, "plugin remove requires an id");
    };
    let path = plugin_registry_path(args);
    let mut config = read_json_file(&path);
    let removed = config
        .get_mut("plugins")
        .and_then(Value::as_object_mut)
        .and_then(|plugins| plugins.remove(&sanitize_identifier(id)))
        .is_some();
    if let Err(error) = write_json_file(&path, &config) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(&json!({"removed": removed, "path": path.to_string_lossy()}))
}

fn plugin_run(args: &[String]) -> CliRunResult {
    let positionals = positional_args(args, &["--workspace", "--dir", "--format"]);
    let Some(id) = positionals.first() else {
        return err_text(2, "plugin run requires an id");
    };
    let command = positionals
        .get(1)
        .cloned()
        .unwrap_or_else(|| "default".to_string());
    let path = plugin_registry_path(args);
    let plugin_id = sanitize_identifier(id);
    let plugin = read_json_file(&path)
        .get("plugins")
        .and_then(|plugins| plugins.get(&plugin_id))
        .cloned()
        .unwrap_or(Value::Null);
    if plugin.is_null() {
        return err_text(1, format!("plugin not installed: {plugin_id}"));
    }
    CliRunResult::ok_json(&json!({
        "plugin_id": plugin_id,
        "command": command,
        "executed": false,
        "reason": "plugin command execution is planned explicitly; registry and manifest resolution are complete",
        "plugin": plugin,
    }))
}

pub(super) fn agent_registry_dir(args: &[String]) -> PathBuf {
    workspace_from_args(args).join(".openagent/agents")
}

fn plugin_registry_path(args: &[String]) -> PathBuf {
    if has_flag(args, &["--global", "-g"]) {
        home_dir().join(".config/openagent/plugins.json")
    } else {
        workspace_from_args(args).join(".openagent/plugins.json")
    }
}

fn plugin_manifest_from_source(source: &str) -> Value {
    let path = PathBuf::from(source);
    if path.is_dir() {
        for relative in [".codex-plugin/plugin.json", "plugin.json"] {
            let value = read_json_file(&path.join(relative));
            if value.as_object().is_some_and(|object| !object.is_empty()) {
                return value;
            }
        }
    } else if path.is_file() {
        let value = read_json_file(&path);
        if value.as_object().is_some_and(|object| !object.is_empty()) {
            return value;
        }
    }
    json!({
        "id": sanitize_identifier(source),
        "name": source,
        "source": source,
        "capabilities": [],
        "commands": {},
    })
}

fn plugin_source_kind(source: &str) -> &'static str {
    let path = PathBuf::from(source);
    if path.exists() {
        "local"
    } else if source.starts_with("http://")
        || source.starts_with("https://")
        || source.contains('/')
    {
        "remote"
    } else {
        "module"
    }
}

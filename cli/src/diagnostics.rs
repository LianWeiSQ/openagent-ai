use super::*;

pub(super) fn stats_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(stats_help());
    }
    let root = session_root_from_args(args);
    let mut session_count = 0_u64;
    let mut run_count = 0_u64;
    let mut input = 0_u64;
    let mut output = 0_u64;
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let state = read_json_file(&path.join("state.latest.json"));
            if state.as_object().is_none_or(Map::is_empty) {
                continue;
            }
            session_count += 1;
            let runs = path.join("runs");
            if let Ok(run_entries) = fs::read_dir(runs) {
                for run_entry in run_entries.flatten() {
                    let summary = read_json_file(&run_entry.path().join("summary.json"));
                    if summary.as_object().is_some_and(|object| !object.is_empty()) {
                        run_count += 1;
                        input += summary
                            .get("total_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        output += summary
                            .get("total_output_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    }
                }
            }
        }
    }
    let payload = json!({"session_root": root.to_string_lossy(), "session_count": session_count, "run_count": run_count, "total_input_tokens": input, "total_output_tokens": output});
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        CliRunResult::ok_json(&payload)
    } else {
        ok_text(render_key_values(
            "Usage Stats",
            &[
                ("Session Root", root.to_string_lossy().to_string()),
                ("Sessions", session_count.to_string()),
                ("Runs", run_count.to_string()),
                ("Input Tokens", input.to_string()),
                ("Output Tokens", output.to_string()),
            ],
        ))
    }
}

pub(super) fn debug_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text("Usage: openagent debug <info|paths|env|sessions|file|rg|bundle>");
    }
    match args[0].as_str() {
        "info" => CliRunResult::ok_json(&json!({
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": env::current_dir().ok().map(|path| path.to_string_lossy().to_string()),
            "provider": active_provider(),
            "session_root": session_root_from_args(args).to_string_lossy(),
        })),
        "paths" => CliRunResult::ok_json(&json!({
            "home": home_dir().to_string_lossy(),
            "models_cache": models_cache_path().to_string_lossy(),
            "auth_file": auth_file_from_args(args).to_string_lossy(),
            "mcp_config": mcp_config_path(args).to_string_lossy(),
            "session_root": session_root_from_args(args).to_string_lossy(),
        })),
        "env" => CliRunResult::ok_json(&json!({"env": sanitized_env()})),
        "sessions" => session_list(&args[1..]),
        "bundle" => debug_bundle(args),
        "file" => {
            let Some(path) = args.get(1) else {
                return err_text(2, "debug file requires a path");
            };
            match fs::read_to_string(path) {
                Ok(text) => ok_text(text),
                Err(error) => err_text(1, error.to_string()),
            }
        }
        "rg" => {
            let Some(pattern) = args.get(1) else {
                return err_text(2, "debug rg requires a pattern");
            };
            run_external_json("rg", &[pattern])
        }
        other => err_text(2, format!("unknown debug command: {other}")),
    }
}

pub(super) fn db_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text("Usage: openagent db <path|summary|rebuild|query|schema|export-sql>");
    }
    match args.first().map(String::as_str).unwrap_or("summary") {
        "path" => ok_text(
            session_root_from_args(args)
                .join("index.jsonl")
                .to_string_lossy(),
        ),
        "summary" => stats_command(args),
        "rebuild" => db_rebuild(args),
        "query" => db_query(args),
        "schema" => ok_text(db_schema_sql()),
        "export-sql" => db_export_sql(args),
        other => err_text(2, format!("unknown db command: {other}")),
    }
}

pub(super) fn lifecycle_command(name: &str, args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(format!("Usage: openagent {name} [--dry-run]"));
    }
    CliRunResult::ok_json(&json!({
        "command": name,
        "performed": false,
        "dry_run": !has_flag(args, &["--yes"]),
        "version": env!("CARGO_PKG_VERSION"),
        "repository": env!("CARGO_PKG_REPOSITORY"),
        "binary": std::env::current_exe().ok().map(|path| path.to_string_lossy().to_string()),
        "plan": lifecycle_plan(name),
        "reason": "OpenAgent is source-tree managed in this workspace; destructive lifecycle changes require --yes and a distribution package.",
    }))
}

pub(super) fn acp_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(
            "Usage: openagent acp <manifest|serve> [--host <host>] [--port <port>] [--cwd <path>]",
        );
    }
    if args.first().map(String::as_str).unwrap_or("manifest") == "manifest" {
        return CliRunResult::ok_json(&json!({
            "schema_version": "openagent.acp_manifest.v1",
            "commands": ["session/new", "session/list", "turn/start", "turn/interrupt", "approval/respond", "question/reply"],
            "transport": {"http": "/api", "sse": "/api/events"},
            "auth": ["bearer", "basic"],
        }));
    }
    let mut runtime_args = args.to_vec();
    if runtime_args.first().map(String::as_str) == Some("serve") {
        runtime_args.remove(0);
    }
    http_runtime_command(&runtime_args, "Usage: openagent acp")
}

pub(super) fn generate_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text("Usage: openagent generate <openapi|commands|acp>");
    }
    if args.first().map(String::as_str) == Some("commands") {
        return CliRunResult::ok_json(&json!({
            "schema_version": "openagent.commands.v1",
            "commands": ["run", "tui", "serve", "models", "agent", "plugin", "github", "pr", "debug", "db", "acp", "generate", "console"],
        }));
    }
    if args.first().map(String::as_str) == Some("acp") {
        return acp_command(&["manifest".to_string()]);
    }
    CliRunResult::ok_json(&json!({
        "openapi": "3.1.0",
        "info": {"title": "OpenAgent Bridge API", "version": env!("CARGO_PKG_VERSION")},
        "paths": {
            "/api/health": {"get": {"operationId": "health"}},
            "/api/events": {"get": {"operationId": "globalEvents"}},
            "/api/sessions": {"get": {"operationId": "listSessions"}, "post": {"operationId": "createSession"}},
            "/api/sessions/{session_id}/turns": {"post": {"operationId": "startTurn"}},
            "/api/turns/{turn_id}/events": {"get": {"operationId": "turnEvents"}},
            "/api/turns/{turn_id}/interrupt": {"post": {"operationId": "interruptTurn"}},
            "/tui/control/next": {"get": {"operationId": "nextTuiControl"}}
        }
    }))
}

pub(super) fn console_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text("Usage: openagent console <login|logout|whoami|orgs|open>");
    }
    let path = home_dir().join(".config/openagent/console.json");
    match args[0].as_str() {
        "login" => {
            let url = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| "https://app.openagent.local".to_string());
            let payload = json!({
                "url": url,
                "profile": {"login": env::var("USER").unwrap_or_else(|_| "local".to_string())},
                "orgs": [{"login": "local", "role": "owner"}],
                "updated_at_ms": now_ms_cli(),
            });
            if let Err(error) = write_json_file(&path, &payload) {
                return err_text(1, error);
            }
            CliRunResult::ok_json(&json!({"logged_in": true, "path": path.to_string_lossy()}))
        }
        "logout" => {
            let removed = fs::remove_file(&path).is_ok();
            CliRunResult::ok_json(&json!({"logged_out": removed}))
        }
        "whoami" | "orgs" | "open" => CliRunResult::ok_json(&read_json_file(&path)),
        other => err_text(2, format!("unknown console command: {other}")),
    }
}

fn debug_bundle(args: &[String]) -> CliRunResult {
    let path = workspace_from_args(args)
        .join(".openagent/debug")
        .join(format!("bundle_{}.json", now_ms_cli()));
    let payload = json!({
        "schema_version": "openagent.debug_bundle.v1",
        "info": {
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": env::current_dir().ok().map(|path| path.to_string_lossy().to_string()),
            "provider": active_provider(),
        },
        "paths": {
            "models_cache": models_cache_path().to_string_lossy(),
            "auth_file": auth_file_from_args(args).to_string_lossy(),
            "session_root": session_root_from_args(args).to_string_lossy(),
        },
        "env": sanitized_env(),
        "stats": stats_payload(&session_root_from_args(args)),
        "created_at_ms": now_ms_cli(),
    });
    if let Err(error) = write_json_file(&path, &payload) {
        return err_text(1, error);
    }
    CliRunResult::ok_json(&json!({"path": path.to_string_lossy(), "bundle": payload}))
}

fn db_rebuild(args: &[String]) -> CliRunResult {
    let root = session_root_from_args(args);
    let rows = session_db_rows(&root);
    let index_path = root.join("index.jsonl");
    if let Some(parent) = index_path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return err_text(1, error.to_string());
    }
    let mut raw = String::new();
    for row in &rows {
        raw.push_str(&stable_json_dumps(row));
        raw.push('\n');
    }
    if let Err(error) = fs::write(&index_path, raw) {
        return err_text(1, error.to_string());
    }
    CliRunResult::ok_json(
        &json!({"rebuilt": true, "path": index_path.to_string_lossy(), "rows": rows.len()}),
    )
}

fn db_query(args: &[String]) -> CliRunResult {
    let root = session_root_from_args(args);
    let query = value_for(args, &["--match", "-m"]).or_else(|| {
        positional_args(
            args,
            &["--workspace", "--dir", "--session-root", "--format"],
        )
        .get(1)
        .cloned()
    });
    let rows = session_db_rows(&root)
        .into_iter()
        .filter(|row| {
            query.as_ref().is_none_or(|needle| {
                stable_json_dumps(row)
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
            })
        })
        .collect::<Vec<_>>();
    CliRunResult::ok_json(&json!({"session_root": root.to_string_lossy(), "rows": rows}))
}

fn db_export_sql(args: &[String]) -> CliRunResult {
    let root = session_root_from_args(args);
    let path = value_for(args, &["--output", "-o"])
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("index.sql"));
    let mut sql = db_schema_sql();
    for row in session_db_rows(&root) {
        sql.push_str(&format!(
            "INSERT INTO sessions(session_id, workspace, status, updated_at_ms, message_count, run_count) VALUES('{}', '{}', '{}', {}, {}, {});\n",
            sql_escape(row.get("session_id").and_then(Value::as_str).unwrap_or_default()),
            sql_escape(row.get("workspace").and_then(Value::as_str).unwrap_or_default()),
            sql_escape(row.get("status").and_then(Value::as_str).unwrap_or_default()),
            row.get("updated_at_ms").and_then(Value::as_u64).unwrap_or_default(),
            row.get("message_count").and_then(Value::as_u64).unwrap_or_default(),
            row.get("run_count").and_then(Value::as_u64).unwrap_or_default(),
        ));
    }
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return err_text(1, error.to_string());
    }
    if let Err(error) = fs::write(&path, sql) {
        return err_text(1, error.to_string());
    }
    CliRunResult::ok_json(&json!({"path": path.to_string_lossy(), "exported": true}))
}

fn db_schema_sql() -> String {
    "CREATE TABLE IF NOT EXISTS sessions(session_id TEXT PRIMARY KEY, workspace TEXT, status TEXT, updated_at_ms INTEGER, message_count INTEGER, run_count INTEGER);\nCREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at_ms DESC);\n".to_string()
}

fn session_db_rows(root: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let state = read_json_file(&path.join("state.latest.json"));
            if state.as_object().is_none_or(Map::is_empty) {
                continue;
            }
            let fallback_id = entry.file_name().to_string_lossy().to_string();
            let run_count = fs::read_dir(path.join("runs"))
                .ok()
                .map(|items| items.flatten().filter(|item| item.path().is_dir()).count())
                .unwrap_or_default();
            rows.push(json!({
                "session_id": state.get("session_id").and_then(Value::as_str).unwrap_or(&fallback_id),
                "workspace": state.get("workspace").cloned().unwrap_or_else(|| json!(".")),
                "status": state.get("status").cloned().unwrap_or_else(|| json!("idle")),
                "updated_at_ms": state.get("updated_at_ms").cloned().unwrap_or_else(|| json!(0)),
                "message_count": state.get("messages").and_then(Value::as_array).map_or(0, Vec::len),
                "run_count": run_count,
            }));
        }
    }
    rows.sort_by(|left, right| {
        right["updated_at_ms"]
            .as_u64()
            .cmp(&left["updated_at_ms"].as_u64())
    });
    rows
}

pub(super) fn stats_payload(root: &Path) -> Value {
    let mut session_count = 0_u64;
    let mut run_count = 0_u64;
    for row in session_db_rows(root) {
        session_count += 1;
        run_count += row
            .get("run_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
    }
    json!({"session_root": root.to_string_lossy(), "session_count": session_count, "run_count": run_count})
}

fn lifecycle_plan(name: &str) -> Value {
    match name {
        "upgrade" => json!([
            "inspect current binary and repository",
            "fetch latest release or git remote",
            "run cargo build/test after upgrade",
            "replace binary only after verification"
        ]),
        "uninstall" => json!([
            "locate binary and config/cache directories",
            "show files that would be removed",
            "require explicit --yes before destructive removal"
        ]),
        _ => json!([]),
    }
}

fn sanitized_env() -> Value {
    let mut envs = Map::new();
    for (key, value) in env::vars() {
        if key.starts_with("OPENAGENT_")
            || key.starts_with("OPENAI_")
            || key.starts_with("ANTHROPIC_")
            || key.ends_with("_API_KEY")
        {
            envs.insert(
                key,
                if looks_secret(&value) {
                    json!(mask_secret(&value))
                } else {
                    json!(value)
                },
            );
        }
    }
    Value::Object(envs)
}

fn sql_escape(value: &str) -> String {
    value.replace('\'', "''")
}

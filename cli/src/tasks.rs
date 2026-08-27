use super::*;
use std::process::Stdio;

pub(super) fn task_command(args: &[String]) -> CliRunResult {
    if args.is_empty() || args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(task_help());
    }
    match args[0].as_str() {
        "list" | "ls" => task_list(&args[1..]),
        "show" | "get" => task_show(&args[1..]),
        "wait" => task_wait(&args[1..]),
        "cancel" | "stop" => task_cancel(&args[1..]),
        "resume" | "retry" => task_resume(&args[1..]),
        "start" => task_start(&args[1..]),
        "worker" => task_worker(&args[1..]),
        other => err_text(2, format!("unknown task command: {other}")),
    }
}

pub(crate) fn spawn_local_background_task_worker(
    source_args: &[String],
    workspace: &Path,
    session_root: &Path,
    provider: Option<&str>,
) -> Result<u32, String> {
    if std::env::var("OPENAGENT_LOCAL_BACKGROUND_WORKER")
        .ok()
        .as_deref()
        .is_some_and(|value| matches!(value, "0" | "false" | "off" | "no"))
    {
        return Ok(0);
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("failed to locate openagent executable: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg("task")
        .arg("worker")
        .arg("--workspace")
        .arg(workspace)
        .arg("--session-root")
        .arg(session_root);
    let mut forwarded = Vec::new();
    copy_cli_options(source_args, &["--agents", "--mcp-config"], &mut forwarded);
    command.args(forwarded);
    let provider = provider
        .map(str::to_string)
        .or_else(|| value_for(source_args, &["--provider"]))
        .unwrap_or_else(active_provider);
    let provider_config = resolve_provider_config(&provider, source_args)?;
    if let Some(api_key) = provider_config.api_key {
        command.env("OPENAGENT_API_KEY", api_key);
    }
    command.env("OPENAGENT_BASE_URL", provider_config.base_url);
    command.env("OPENAGENT_WIRE_API", provider_config.wire_api);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|child| child.id())
        .map_err(|error| format!("failed to start local background task worker: {error}"))
}

fn task_list(args: &[String]) -> CliRunResult {
    let config = task_runtime_config(args);
    let root = session_root_from_args(args);
    let session_id =
        value_for(args, &["--session", "-s"]).or_else(|| latest_primary_session_id(&root));
    let Some(session_id) = session_id else {
        return err_text(2, "task list requires --session <parent_session_id>");
    };
    if !valid_session_id(&session_id) {
        return err_text(2, "Invalid session id");
    }
    let payload = openagent_http_runtime::local_session_tasks(&config, &session_id);
    render_task_result(args, payload)
}

fn task_show(args: &[String]) -> CliRunResult {
    let Some(task_id) = task_id_from_args(args) else {
        return err_text(2, "task show requires <task_id>");
    };
    match load_task_state(args, &task_id) {
        Ok((_, state)) => render_task_result(args, state),
        Err(error) => err_text(1, error),
    }
}

fn task_wait(args: &[String]) -> CliRunResult {
    let Some(task_id) = task_id_from_args(args) else {
        return err_text(2, "task wait requires <task_id>");
    };
    let (parent_session_id, _) = match load_task_state(args, &task_id) {
        Ok(value) => value,
        Err(error) => return err_text(1, error),
    };
    let timeout_ms = value_for(args, &["--timeout-ms"])
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5_000);
    match openagent_http_runtime::local_wait_session_task(
        &task_runtime_config(args),
        &parent_session_id,
        &task_id,
        timeout_ms,
    ) {
        Ok(payload) => render_task_result(args, payload),
        Err(error) => err_text(1, error),
    }
}

fn task_cancel(args: &[String]) -> CliRunResult {
    let Some(task_id) = task_id_from_args(args) else {
        return err_text(2, "task cancel requires <task_id>");
    };
    let (parent_session_id, _) = match load_task_state(args, &task_id) {
        Ok(value) => value,
        Err(error) => return err_text(1, error),
    };
    match openagent_http_runtime::local_cancel_session_task(
        &task_runtime_config(args),
        &parent_session_id,
        &task_id,
    ) {
        Ok(payload) => render_task_result(args, payload),
        Err(error) => err_text(1, error),
    }
}

fn task_resume(args: &[String]) -> CliRunResult {
    let Some(task_id) = task_id_from_args(args) else {
        return err_text(2, "task resume requires <task_id>");
    };
    let (parent_session_id, state) = match load_task_state(args, &task_id) {
        Ok(value) => value,
        Err(error) => return err_text(1, error),
    };
    let config = task_runtime_config(args);
    let payload = match openagent_http_runtime::local_resume_session_task(
        &config,
        &parent_session_id,
        &task_id,
    ) {
        Ok(payload) => payload,
        Err(error) => return err_text(1, error),
    };
    let workspace = state
        .get("workspace")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_from_args(args));
    if let Err(error) = spawn_local_background_task_worker(
        args,
        &workspace,
        &session_root_from_args(args),
        state.pointer("/metadata/provider").and_then(Value::as_str),
    ) {
        return err_text(
            1,
            format!("task requeued but worker did not start: {error}"),
        );
    }
    render_task_result(args, payload)
}

fn task_start(args: &[String]) -> CliRunResult {
    let Some(task_id) = task_id_from_args(args) else {
        return err_text(2, "task start requires <task_id>");
    };
    let (_, state) = match load_task_state(args, &task_id) {
        Ok(value) => value,
        Err(error) => return err_text(1, error),
    };
    let status = state
        .pointer("/metadata/task_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if status != "queued" {
        return err_text(1, format!("task is not queued: {status}"));
    }
    let workspace = state
        .get("workspace")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_from_args(args));
    match spawn_local_background_task_worker(
        args,
        &workspace,
        &session_root_from_args(args),
        state.pointer("/metadata/provider").and_then(Value::as_str),
    ) {
        Ok(worker_pid) => render_task_result(
            args,
            json!({"task_id": task_id, "status": "queued", "worker_pid": worker_pid}),
        ),
        Err(error) => err_text(1, error),
    }
}

fn task_worker(args: &[String]) -> CliRunResult {
    let config = task_runtime_config(args);
    match openagent_http_runtime::run_local_background_task_worker_once(&config) {
        Ok(()) => CliRunResult::ok_json(&json!({"completed": true})),
        Err(error) => err_text(1, error),
    }
}

fn task_runtime_config(args: &[String]) -> openagent_http_runtime::HttpRuntimeConfig {
    let mut runtime_args = args.to_vec();
    if value_for(&runtime_args, &["--workspace"]).is_none() {
        runtime_args.push("--workspace".to_string());
        runtime_args.push(workspace_from_args(args).to_string_lossy().to_string());
    }
    if value_for(&runtime_args, &["--session-root"]).is_none() {
        runtime_args.push("--session-root".to_string());
        runtime_args.push(session_root_from_args(args).to_string_lossy().to_string());
    }
    openagent_http_runtime::parse_cli_args(&runtime_args).0
}

fn task_id_from_args(args: &[String]) -> Option<String> {
    positional_args(args, RUN_POSITIONAL_VALUE_FLAGS)
        .first()
        .cloned()
}

fn load_task_state(args: &[String], task_id: &str) -> Result<(String, Value), String> {
    if !valid_session_id(task_id) {
        return Err("Invalid task id".to_string());
    }
    let state = read_json_file(
        &session_root_from_args(args)
            .join(task_id)
            .join("state.latest.json"),
    );
    if state.as_object().is_none_or(Map::is_empty) {
        return Err(format!("task state not found: {task_id}"));
    }
    if state.pointer("/metadata/subagent").and_then(Value::as_bool) != Some(true) {
        return Err(format!("session is not a subagent task: {task_id}"));
    }
    let parent_session_id = state
        .pointer("/metadata/parent_session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("task has no parent session: {task_id}"))?
        .to_string();
    Ok((parent_session_id, state))
}

fn latest_primary_session_id(root: &Path) -> Option<String> {
    let mut candidates = fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let state = read_json_file(&entry.path().join("state.latest.json"));
            if state.as_object().is_none_or(Map::is_empty)
                || state.pointer("/metadata/subagent").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            Some((
                state
                    .get("updated_at_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                state
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string()),
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates.into_iter().next().map(|(_, id)| id)
}

fn render_task_result(args: &[String], payload: Value) -> CliRunResult {
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        return CliRunResult::ok_json(&payload);
    }
    if let Some(tasks) = payload.get("flat_tasks").and_then(Value::as_array) {
        let rows = tasks
            .iter()
            .map(|task| {
                vec![
                    task.get("task_id")
                        .or_else(|| task.get("session_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("-")
                        .to_string(),
                    task.get("canonical_status")
                        .or_else(|| task.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    task.get("subagent_type")
                        .or_else(|| task.get("agent"))
                        .and_then(Value::as_str)
                        .unwrap_or("subagent")
                        .to_string(),
                    task.get("title")
                        .or_else(|| task.get("description"))
                        .and_then(Value::as_str)
                        .unwrap_or("task")
                        .to_string(),
                ]
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return ok_text("Tasks: none");
        }
        return ok_text(render_table(
            &["Task", "Status", "Agent", "Description"],
            &rows,
        ));
    }
    ok_text(serde_json::to_string_pretty(&payload).unwrap_or_else(|_| stable_json_dumps(&payload)))
}

fn task_help() -> &'static str {
    "Usage: openagent task <list|show|wait|start|cancel|resume> [task_id] [options]\n\n\
     list:   --session <parent_session_id> --session-root <path> [--format json]\n\
     show:   <task_id> --session-root <path> [--format json]\n\
     wait:   <task_id> --timeout-ms <n> --session-root <path>\n\
     start:  <task_id> --session-root <path>\n\
     cancel: <task_id> --session-root <path>\n\
     resume: <task_id> --session-root <path>"
}

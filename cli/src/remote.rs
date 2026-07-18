use super::*;

use openagent_bridge_server_client::{
    RemoteAuth as BridgeRemoteAuth, RemoteRuntimeClient as BridgeRuntimeClient,
};

#[derive(Clone, Debug, Default)]
pub(super) struct RemoteAuth {
    token: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

pub(super) fn remote_auth_from_args(args: &[String]) -> RemoteAuth {
    RemoteAuth {
        token: value_for(args, &["--server-token"])
            .or_else(|| env::var(DEFAULT_SERVER_TOKEN_ENV).ok())
            .or_else(|| {
                value_for(args, &["--server-token-env"]).and_then(|name| env::var(name).ok())
            }),
        username: value_for(args, &["--username", "-u"]),
        password: value_for(args, &["--password", "-p"]),
    }
}

fn bridge_auth(auth: &RemoteAuth) -> BridgeRemoteAuth {
    if let Some(token) = auth.token.as_deref().filter(|value| !value.is_empty()) {
        return BridgeRemoteAuth::bearer(token);
    }
    if let Some(password) = auth.password.as_deref().filter(|value| !value.is_empty()) {
        return BridgeRemoteAuth::basic(auth.username.as_deref().unwrap_or("openagent"), password);
    }
    BridgeRemoteAuth::default()
}

pub(super) fn bridge_client(server_url: &str, auth: &RemoteAuth) -> BridgeRuntimeClient {
    BridgeRuntimeClient::new(server_url).with_auth(bridge_auth(auth))
}

pub(super) fn remote_select_session(
    server_url: &str,
    token: Option<&str>,
    explicit: Option<String>,
    continue_last: bool,
    workspace: &Path,
) -> Result<String, String> {
    if let Some(session_id) = explicit {
        return Ok(session_id);
    }
    if continue_last {
        let payload = http_json("GET", server_url, "/api/sessions", token, None)?;
        if let Some(session_id) = payload
            .get("sessions")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("session_id").or_else(|| item.get("id")))
            .and_then(Value::as_str)
        {
            return Ok(session_id.to_string());
        }
    }
    let payload = http_json(
        "POST",
        server_url,
        "/api/sessions",
        token,
        Some(json!({"cwd": workspace.to_string_lossy()})),
    )?;
    payload
        .get("session_id")
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "server did not return a session id".to_string())
}

pub(super) fn remote_select_session_with_auth(
    server_url: &str,
    auth: &RemoteAuth,
    explicit: Option<String>,
    continue_last: bool,
    fork: bool,
    workspace: &Path,
) -> Result<String, String> {
    if fork && explicit.is_none() && !continue_last {
        return Err("--fork requires --continue or --session".to_string());
    }
    let base = if let Some(session_id) = explicit {
        Some(session_id)
    } else if continue_last {
        let payload = http_json_with_auth("GET", server_url, "/api/sessions", auth, None)?;
        payload
            .get("sessions")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("session_id").or_else(|| item.get("id")))
            .and_then(Value::as_str)
            .map(str::to_string)
    } else {
        None
    };
    if !fork && let Some(session_id) = base {
        return Ok(session_id);
    }
    let mut body = json!({"cwd": workspace.to_string_lossy()});
    if let Some(fork_from) = base {
        body["fork_from"] = json!(fork_from);
    }
    let payload = http_json_with_auth("POST", server_url, "/api/sessions", auth, Some(body))?;
    payload
        .get("session_id")
        .or_else(|| payload.get("id"))
        .or_else(|| payload.get("session").and_then(|session| session.get("id")))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "server did not return a session id".to_string())
}

pub(super) fn remote_start_turn_request(
    server_url: &str,
    token: Option<&str>,
    session_id: &str,
    request: &RemoteTurnRequest,
) -> Result<Value, String> {
    http_json(
        "POST",
        server_url,
        &format!("/api/sessions/{session_id}/turns"),
        token,
        Some(request.to_payload()),
    )
}

pub(super) fn remote_start_turn_with_auth(
    server_url: &str,
    auth: &RemoteAuth,
    session_id: &str,
    prompt: &str,
) -> Result<Value, String> {
    remote_start_turn_request_with_auth(
        server_url,
        auth,
        session_id,
        &RemoteTurnRequest::new(prompt),
    )
}

pub(super) fn remote_start_turn_request_with_auth(
    server_url: &str,
    auth: &RemoteAuth,
    session_id: &str,
    request: &RemoteTurnRequest,
) -> Result<Value, String> {
    http_json_with_auth(
        "POST",
        server_url,
        &format!("/api/sessions/{session_id}/turns"),
        auth,
        Some(request.to_payload()),
    )
}

fn remote_turn_events(
    server_url: &str,
    auth: &RemoteAuth,
    turn_id: &str,
    last_event_id: u64,
) -> Result<Vec<Value>, String> {
    let mut events = Vec::new();
    bridge_client(server_url, auth).turn_events_live_stream(
        turn_id,
        last_event_id,
        remote_attach_poll_duration(),
        |event| {
            events.push(event);
            Ok(())
        },
    )?;
    Ok(events)
}

pub(super) fn remote_events_for_payload(
    server_url: &str,
    auth: &RemoteAuth,
    payload: &Value,
) -> Result<Vec<Value>, String> {
    let fallback = payload
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(turn_id) = remote_turn_id(payload) else {
        return Ok(fallback);
    };
    let mut events = Vec::new();
    let mut seen = BTreeSet::new();
    let mut last_id = 0_u64;
    for event in fallback {
        last_id = last_id.max(bridge_event_sequence(&event));
        if let Some(key) = bridge_event_dedupe_key(&event)
            && !seen.insert(key)
        {
            continue;
        }
        events.push(event);
    }
    let deadline = SystemTime::now() + Duration::from_secs(remote_attach_wait_seconds());
    loop {
        match remote_turn_events(server_url, auth, &turn_id, last_id) {
            Ok(next) => {
                let mut advanced = false;
                for event in next {
                    let seq = bridge_event_sequence(&event);
                    if seq > last_id {
                        last_id = seq;
                    }
                    if let Some(key) = bridge_event_dedupe_key(&event)
                        && !seen.insert(key)
                    {
                        continue;
                    }
                    advanced = true;
                    events.push(event);
                }
                if events.iter().any(is_terminal_bridge_event) {
                    return Ok(events);
                }
                if !advanced && SystemTime::now() >= deadline {
                    return Ok(events);
                }
            }
            Err(error) if events.is_empty() => return Err(error),
            Err(_) => return Ok(events),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn remote_attach_wait_seconds() -> u64 {
    env::var("OPENAGENT_ATTACH_WAIT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
}

fn remote_attach_poll_duration() -> Duration {
    let millis = env::var("OPENAGENT_ATTACH_POLL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(250)
        .clamp(250, 5_000);
    Duration::from_millis(millis)
}

fn bridge_event_sequence(event: &Value) -> u64 {
    event
        .get("sequence")
        .or_else(|| event.get("global_sequence"))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn bridge_event_global_sequence(event: &Value) -> u64 {
    event
        .get("global_sequence")
        .or_else(|| event.get("sequence"))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn bridge_event_dedupe_key(event: &Value) -> Option<String> {
    if let Some(event_id) = event.get("event_id").and_then(Value::as_str)
        && !event_id.is_empty()
    {
        return Some(format!("event_id:{event_id}"));
    }
    Some(format!(
        "{}:{}:{}",
        bridge_event_sequence(event),
        event.get("method").and_then(Value::as_str).unwrap_or(""),
        stable_json_dumps(event.get("params").unwrap_or(&Value::Null))
    ))
}

fn is_terminal_bridge_event(event: &Value) -> bool {
    matches!(
        event.get("method").and_then(Value::as_str),
        Some("turn/completed" | "turn/failed" | "turn/interrupted")
    ) || matches!(
        event
            .get("params")
            .and_then(|params| params.get("status"))
            .and_then(Value::as_str),
        Some("completed" | "failed" | "interrupted")
    )
}

fn remote_turn_id(payload: &Value) -> Option<String> {
    payload
        .get("turn_id")
        .or_else(|| payload.get("turn").and_then(|turn| turn.get("id")))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn remote_list_sessions(server_url: &str, auth: &RemoteAuth) -> Result<Vec<Value>, String> {
    let payload = http_json_with_auth("GET", server_url, "/api/sessions", auth, None)?;
    Ok(payload
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn remote_tasks_payload(
    server_url: &str,
    auth: &RemoteAuth,
    session_id: &str,
) -> Result<Value, String> {
    http_json_with_auth(
        "GET",
        server_url,
        &format!("/api/sessions/{session_id}/tasks"),
        auth,
        None,
    )
}

fn remote_sessions_text(sessions: &[Value]) -> String {
    if sessions.is_empty() {
        return "Remote sessions: none\n".to_string();
    }
    let mut text = String::from("Remote sessions:\n");
    for (index, session) in sessions.iter().take(20).enumerate() {
        let id = session
            .get("session_id")
            .or_else(|| session.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let status = session
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let messages = session
            .get("message_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let workspace = session
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or(".");
        text.push_str(&format!(
            "  {}. {}  status={}  messages={}  workspace={}\n",
            index + 1,
            id,
            status,
            messages,
            workspace
        ));
    }
    if sessions.len() > 20 {
        text.push_str(&format!("  ... {} more\n", sessions.len() - 20));
    }
    text
}

fn remote_tasks_text(payload: &Value) -> String {
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let tree = payload
        .get("tree")
        .and_then(Value::as_array)
        .or_else(|| payload.get("tasks").and_then(Value::as_array));
    let Some(tasks) = tree else {
        return format!("Remote tasks for {session_id}: none\n");
    };
    if tasks.is_empty() {
        return format!("Remote tasks for {session_id}: none\n");
    }
    let mut text = format!("Remote tasks for {session_id}:\n");
    for task in tasks {
        append_remote_task_text(&mut text, task, 1);
    }
    text
}

fn append_remote_task_text(text: &mut String, task: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    let id = task
        .get("session_id")
        .or_else(|| task.get("task_id"))
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let title = task
        .get("title")
        .or_else(|| task.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("task");
    let status = task
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let subagent = task
        .get("subagent_type")
        .or_else(|| task.get("agent"))
        .and_then(Value::as_str)
        .unwrap_or("subagent");
    let background = task
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let depth_value = task
        .get("task_depth")
        .and_then(Value::as_u64)
        .unwrap_or(depth as u64);
    let marker = if background { " background" } else { "" };
    text.push_str(&format!(
        "{indent}- {id}  [{status}] {subagent}  depth={depth_value}{marker}  {title}\n"
    ));
    if let Some(children) = task.get("children").and_then(Value::as_array) {
        for child in children {
            append_remote_task_text(text, child, depth + 1);
        }
    }
}

fn http_json(
    method: &str,
    server_url: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<Value, String> {
    http_json_with_auth(
        method,
        server_url,
        path,
        &RemoteAuth {
            token: token.map(str::to_string),
            username: None,
            password: None,
        },
        body,
    )
}

fn http_json_with_auth(
    method: &str,
    server_url: &str,
    path: &str,
    auth: &RemoteAuth,
    body: Option<Value>,
) -> Result<Value, String> {
    let raw = http_text_with_auth(method, server_url, path, auth, body)?;
    serde_json::from_str(&raw).map_err(|error| format!("server response was not JSON: {error}"))
}

fn http_text_with_auth(
    method: &str,
    server_url: &str,
    path: &str,
    auth: &RemoteAuth,
    body: Option<Value>,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("{}{}", server_url.trim_end_matches('/'), path);
    let mut request = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        _ => return Err(format!("unsupported HTTP method: {method}")),
    };
    if let Some(token) = auth.token.as_deref().filter(|value| !value.is_empty()) {
        request = request.bearer_auth(token);
    }
    if let Some(password) = auth.password.as_deref().filter(|value| !value.is_empty()) {
        request = request.basic_auth(
            auth.username.as_deref().unwrap_or("openagent"),
            Some(password),
        );
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .map_err(|error| format!("{method} {path} failed: {error}"))?;
    let status = response.status();
    let raw = response.text().map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "{method} {path} returned HTTP {}: {raw}",
            status.as_u16()
        ));
    }
    Ok(raw)
}

pub(super) fn text_from_bridge_events(events: &[Value]) -> String {
    let mut text = String::new();
    let mut final_answer = String::new();
    for event in events {
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = event.get("params").unwrap_or(&Value::Null);
        if method == "item/agentMessage/delta" {
            if let Some(delta) = params
                .get("delta")
                .or_else(|| params.get("text"))
                .or_else(|| params.get("event").and_then(|event| event.get("text")))
                .and_then(Value::as_str)
            {
                text.push_str(delta);
            }
        }
        if method == "turn/approval_requested" {
            let tool = params
                .get("approval")
                .and_then(|approval| approval.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            text.push_str(&format!("\n[approval required: {tool}]\n"));
        }
        if method == "turn/interrupted" {
            text.push_str("\n[turn interrupted]\n");
        }
        if method == "turn/completed" {
            final_answer = params
                .get("final_answer")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
    }
    if text.is_empty() { final_answer } else { text }
}

pub(super) fn http_runtime_command(args: &[String], help: &'static str) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(help);
    }
    let result = openagent_http_runtime::run_cli(args);
    CliRunResult {
        exit_code: result.exit_code,
        stdout: result.stdout,
        stderr: result.stderr,
    }
}

pub(super) fn tui_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(tui_help());
    }
    if value_for(args, &["--attach"]).is_some() {
        return attach_command(args);
    }
    if let Some(prompt) = value_for(args, &["--prompt"]) {
        let mut run_args = vec!["--skip-doctor".to_string()];
        run_args.extend(args.iter().filter(|arg| *arg != "--prompt").cloned());
        run_args.push(prompt);
        return run_prompt_command(&run_args);
    }
    if !io::stdin().is_terminal() {
        return ok_text("openagent-tui ready; pass --prompt or use an interactive terminal");
    }
    interactive_local_loop(args)
}

pub(super) fn terminal_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(terminal_help());
    }
    let server_url = value_for(args, &["--server-url", "--attach"])
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
    let format = value_for(args, &["--format"]).unwrap_or_else(|| "text".to_string());
    if !matches!(format.as_str(), "text" | "json") {
        return err_text(2, "openagent terminal --format must be text or json");
    }
    let timeout_ms = match value_for(args, &["--timeout-ms"]) {
        Some(value) => match value.parse::<u64>() {
            Ok(parsed) => Some(parsed),
            Err(_) => return err_text(2, "--timeout-ms must be an integer"),
        },
        None => None,
    };
    let Some(command) = terminal_command_text(args) else {
        return err_text(
            2,
            "openagent terminal requires --command or command arguments after --",
        );
    };
    let cwd = value_for(args, &["--cwd", "--workspace", "--dir"]).map(PathBuf::from);
    let client_timeout = Duration::from_millis(
        timeout_ms
            .unwrap_or(10_000)
            .saturating_add(1_000)
            .max(5_000),
    );
    let client = BridgeRuntimeClient::new(server_url)
        .with_auth(bridge_auth(&remote_auth_from_args(args)))
        .with_timeout(client_timeout);
    let payload = match client.terminal_run(&command, cwd.as_deref(), timeout_ms) {
        Ok(payload) => payload,
        Err(error) => return err_text(1, error),
    };
    let exit_code = terminal_exit_code(&payload);
    if format == "json" {
        return CliRunResult {
            exit_code,
            stdout: format!("{}\n", stable_json_dumps(&payload)),
            stderr: String::new(),
        };
    }
    CliRunResult {
        exit_code,
        stdout: payload
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        stderr: payload
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

fn terminal_command_text(args: &[String]) -> Option<String> {
    value_for(args, &["--command"]).or_else(|| {
        let positionals = positional_args(
            args,
            &[
                "--server-url",
                "--attach",
                "--server-token",
                "--server-token-env",
                "--username",
                "-u",
                "--password",
                "-p",
                "--cwd",
                "--workspace",
                "--dir",
                "--timeout-ms",
                "--format",
                "--command",
            ],
        );
        let command = positionals.join(" ");
        (!command.trim().is_empty()).then_some(command)
    })
}

fn terminal_exit_code(payload: &Value) -> i32 {
    if payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return 0;
    }
    payload
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value != 0)
        .unwrap_or(1)
}

fn remote_terminal_run_text(
    url: &str,
    auth: &RemoteAuth,
    command: &str,
    cwd: Option<&Path>,
) -> Result<String, String> {
    let client = BridgeRuntimeClient::new(url)
        .with_auth(bridge_auth(auth))
        .with_timeout(Duration::from_secs(31));
    let payload = client.terminal_run(command, cwd, None)?;
    Ok(terminal_payload_text(&payload))
}

fn terminal_payload_text(payload: &Value) -> String {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("terminal");
    let exit_code = payload
        .get("exit_code")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let cwd = payload
        .get("cwd_relative")
        .or_else(|| payload.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let mut text = format!("Terminal `{command}` in {cwd} exited {exit_code}\n");
    if let Some(stdout) = payload.get("stdout").and_then(Value::as_str)
        && !stdout.is_empty()
    {
        text.push_str(stdout);
        if !stdout.ends_with('\n') {
            text.push('\n');
        }
    }
    if let Some(stderr) = payload.get("stderr").and_then(Value::as_str)
        && !stderr.is_empty()
    {
        text.push_str("[stderr]\n");
        text.push_str(stderr);
        if !stderr.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

fn slash_terminal_command(command: &str) -> Option<&str> {
    ["/terminal", "/term"].into_iter().find_map(|prefix| {
        if command == prefix {
            return Some("");
        }
        command
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix(' '))
            .map(str::trim)
    })
}

pub(super) fn attach_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(attach_help());
    }
    let positionals = positional_args(
        args,
        &[
            "--workspace",
            "--dir",
            "--session",
            "-s",
            "--server-token",
            "--server-token-env",
            "--username",
            "-u",
            "--password",
            "-p",
            "--attach",
            "--format",
        ],
    );
    let Some(url) = value_for(args, &["--attach"]).or_else(|| positionals.first().cloned()) else {
        return err_text(2, "openagent attach requires a server URL");
    };
    let auth = remote_auth_from_args(args);
    let mut health = json!({});
    if !has_flag(args, &["--skip-health-check"])
        && let Err(error) =
            http_json_with_auth("GET", &url, "/api/health", &auth, None).map(|payload| {
                health = payload;
            })
    {
        return err_text(1, error);
    }
    if !io::stdin().is_terminal() {
        let sessions = remote_list_sessions(&url, &auth).unwrap_or_default();
        if value_for(args, &["--format"]).as_deref() == Some("json") {
            return CliRunResult::ok_json(&json!({
                "attached": true,
                "server_url": url,
                "health": health,
                "sessions": sessions,
            }));
        }
        let mut output = format!("attached to {url}\n");
        output.push_str(&remote_sessions_text(&sessions));
        return ok_text(output);
    }
    interactive_remote_loop(args, &url, &auth)
}

fn interactive_local_loop(args: &[String]) -> CliRunResult {
    let mut stdout = String::new();
    stdout.push_str("OpenAgent TUI direct mode. Type /exit to quit.\n");
    let mut line = String::new();
    loop {
        line.clear();
        if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let prompt = line.trim();
        if matches!(prompt, "/exit" | "/quit") {
            break;
        }
        if prompt.is_empty() {
            continue;
        }
        let mut run_args = args.to_vec();
        run_args.push("--skip-doctor".to_string());
        run_args.push(prompt.to_string());
        let result = run_prompt_command(&run_args);
        stdout.push_str(&result.stdout);
        if result.exit_code != 0 {
            return CliRunResult {
                exit_code: result.exit_code,
                stdout,
                stderr: result.stderr,
            };
        }
    }
    ok_text(stdout)
}

fn interactive_remote_loop(args: &[String], url: &str, auth: &RemoteAuth) -> CliRunResult {
    let workspace = workspace_from_args(args);
    let mut current_session = match remote_select_session_with_auth(
        url,
        auth,
        value_for(args, &["--session", "-s"]),
        has_flag(args, &["--continue", "-c"]),
        has_flag(args, &["--fork"]),
        &workspace,
    ) {
        Ok(session_id) => Some(session_id),
        Err(error) if has_flag(args, &["--continue", "-c"]) || has_flag(args, &["--fork"]) => {
            return err_text(1, error);
        }
        Err(_) => None,
    };
    if io::stdout().is_terminal() {
        let handler = RemoteTerminalHandler {
            url: url.to_string(),
            auth: auth.clone(),
            workspace,
            current_session,
            last_turn_id: None,
            last_global_event_id: 0,
            pending_events: Vec::new(),
            seen_events: BTreeSet::new(),
        };
        return match openagent_tui::run_terminal_ui(
            openagent_tui::TerminalUiOptions {
                title: format!("OpenAgent remote attach: {url}"),
                status: "connected".to_string(),
            },
            handler,
        ) {
            Ok(()) => ok_text(""),
            Err(error) => err_text(1, error),
        };
    }
    let mut last_turn_id: Option<String> = None;
    let mut stdout = format!(
        "OpenAgent remote attach: {url}\nCommands: /sessions, /tasks, /task <id>, /terminal <command>, /resume <id>, /new, /fork, /interrupt [turn_id], /exit\n"
    );
    if let Ok(sessions) = remote_list_sessions(url, auth) {
        stdout.push_str(&remote_sessions_text(&sessions));
    }
    if let Some(session_id) = current_session.as_deref() {
        stdout.push_str(&format!("Current session: {session_id}\n"));
    }
    let mut line = String::new();
    loop {
        line.clear();
        if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let prompt = line.trim();
        if matches!(prompt, "/exit" | "/quit") {
            break;
        }
        if prompt.is_empty() {
            continue;
        }
        if prompt == "/sessions" {
            match remote_list_sessions(url, auth) {
                Ok(sessions) => stdout.push_str(&remote_sessions_text(&sessions)),
                Err(error) => return err_text(1, error),
            }
            continue;
        }
        if prompt == "/tasks" {
            let Some(session_id) = current_session.as_deref() else {
                stdout.push_str("No current session. Use /new or /resume <session_id>.\n");
                continue;
            };
            match remote_tasks_payload(url, auth, session_id) {
                Ok(payload) => stdout.push_str(&remote_tasks_text(&payload)),
                Err(error) => return err_text(1, error),
            }
            continue;
        }
        if let Some(command) = slash_terminal_command(prompt) {
            if command.is_empty() {
                stdout.push_str("Usage: /terminal <command>\n");
            } else {
                match remote_terminal_run_text(url, auth, command, None) {
                    Ok(text) => stdout.push_str(&text),
                    Err(error) => return err_text(1, error),
                }
            }
            continue;
        }
        if let Some(task_id) = prompt.strip_prefix("/task ").map(str::trim) {
            if task_id.is_empty() {
                stdout.push_str("Usage: /task <task_session_id>\n");
            } else {
                current_session = Some(task_id.to_string());
                stdout.push_str(&format!("Current task session: {task_id}\n"));
            }
            continue;
        }
        if let Some(session_id) = prompt.strip_prefix("/resume ").map(str::trim) {
            if session_id.is_empty() {
                stdout.push_str("Usage: /resume <session_id>\n");
            } else {
                current_session = Some(session_id.to_string());
                stdout.push_str(&format!("Current session: {session_id}\n"));
            }
            continue;
        }
        if prompt == "/new" {
            match remote_select_session_with_auth(url, auth, None, false, false, &workspace) {
                Ok(session_id) => {
                    stdout.push_str(&format!("Created session: {session_id}\n"));
                    current_session = Some(session_id);
                }
                Err(error) => return err_text(1, error),
            }
            continue;
        }
        if prompt == "/fork" {
            let Some(base) = current_session.clone() else {
                stdout.push_str("No current session to fork. Use /new or /resume <session_id>.\n");
                continue;
            };
            match remote_select_session_with_auth(url, auth, Some(base), false, true, &workspace) {
                Ok(session_id) => {
                    stdout.push_str(&format!("Forked session: {session_id}\n"));
                    current_session = Some(session_id);
                }
                Err(error) => return err_text(1, error),
            }
            continue;
        }
        if prompt == "/interrupt" || prompt.starts_with("/interrupt ") {
            let turn_id = prompt
                .strip_prefix("/interrupt ")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| last_turn_id.clone());
            let Some(turn_id) = turn_id else {
                stdout.push_str("No turn to interrupt. Pass /interrupt <turn_id>.\n");
                continue;
            };
            match http_json_with_auth(
                "POST",
                url,
                &format!("/api/turns/{turn_id}/interrupt"),
                auth,
                None,
            ) {
                Ok(payload) => {
                    if let Some(events) = payload.get("events").and_then(Value::as_array) {
                        stdout.push_str(&text_from_bridge_events(events));
                        stdout.push('\n');
                    } else {
                        stdout.push_str(&stable_json_dumps(&payload));
                        stdout.push('\n');
                    }
                }
                Err(error) => return err_text(1, error),
            }
            continue;
        }
        let session_id = match current_session.clone() {
            Some(session_id) => session_id,
            None => {
                match remote_select_session_with_auth(url, auth, None, false, false, &workspace) {
                    Ok(session_id) => {
                        stdout.push_str(&format!("Created session: {session_id}\n"));
                        current_session = Some(session_id.clone());
                        session_id
                    }
                    Err(error) => return err_text(1, error),
                }
            }
        };
        match remote_start_turn_with_auth(url, auth, &session_id, prompt) {
            Ok(payload) => {
                last_turn_id = remote_turn_id(&payload).or(last_turn_id);
                match remote_events_for_payload(url, auth, &payload) {
                    Ok(events) if !events.is_empty() => {
                        stdout.push_str(&text_from_bridge_events(&events));
                        stdout.push('\n');
                    }
                    Ok(_) => {
                        stdout.push_str(&stable_json_dumps(&payload));
                        stdout.push('\n');
                    }
                    Err(error) => return err_text(1, error),
                }
            }
            Err(error) => return err_text(1, error),
        }
    }
    ok_text(stdout)
}

struct RemoteTerminalHandler {
    url: String,
    auth: RemoteAuth,
    workspace: PathBuf,
    current_session: Option<String>,
    last_turn_id: Option<String>,
    last_global_event_id: u64,
    pending_events: Vec<Value>,
    seen_events: BTreeSet<String>,
}

impl RemoteTerminalHandler {
    fn ensure_session(&mut self) -> Result<String, String> {
        if let Some(session_id) = self.current_session.clone() {
            return Ok(session_id);
        }
        let session_id = remote_select_session_with_auth(
            &self.url,
            &self.auth,
            None,
            false,
            false,
            &self.workspace,
        )?;
        self.current_session = Some(session_id.clone());
        Ok(session_id)
    }

    fn remember_payload_events(&mut self, payload: &Value) {
        let events = payload
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let events = self.filter_new_events(events);
        self.pending_events.extend(events);
    }

    fn filter_new_events(&mut self, events: Vec<Value>) -> Vec<Value> {
        let mut output = Vec::new();
        for event in events {
            let sequence = bridge_event_global_sequence(&event);
            if sequence > self.last_global_event_id {
                self.last_global_event_id = sequence;
            }
            if let Some(key) = bridge_event_dedupe_key(&event)
                && !self.seen_events.insert(key)
            {
                continue;
            }
            output.push(event);
        }
        output
    }
}

impl openagent_tui::TerminalEventHandler for RemoteTerminalHandler {
    fn initial_lines(&mut self) -> Vec<openagent_tui::TimelineLine> {
        let mut lines = tui_lines("status", format!("connected to {}", self.url), true);
        match remote_list_sessions(&self.url, &self.auth) {
            Ok(sessions) => {
                lines.extend(tui_lines("status", remote_sessions_text(&sessions), false))
            }
            Err(error) => lines.extend(tui_lines("warning", error, true)),
        }
        if let Some(session_id) = self.current_session.as_deref() {
            lines.extend(tui_lines(
                "status",
                format!("current session: {session_id}"),
                true,
            ));
        }
        lines
    }

    fn poll_bridge_events(&mut self) -> Result<Vec<Value>, String> {
        let mut events = Vec::new();
        bridge_client(&self.url, &self.auth).global_events_live_stream(
            self.last_global_event_id,
            remote_attach_poll_duration(),
            |event| {
                events.push(event);
                Ok(())
            },
        )?;
        Ok(self.filter_new_events(events))
    }

    fn poll_control_request(&mut self) -> Result<Option<Value>, String> {
        let payload = http_json_with_auth("GET", &self.url, "/tui/control/next", &self.auth, None)?;
        let path = payload
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if path.is_empty() {
            return Ok(None);
        }
        Ok(Some(payload))
    }

    fn record_control_response(&mut self, payload: &Value) -> Result<(), String> {
        http_json_with_auth(
            "POST",
            &self.url,
            "/tui/control/response",
            &self.auth,
            Some(payload.clone()),
        )
        .map(|_| ())
    }

    fn drain_bridge_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.pending_events)
    }

    fn handle_submit(&mut self, prompt: &str) -> Result<Vec<openagent_tui::TimelineLine>, String> {
        let session_id = self.ensure_session()?;
        let payload = remote_start_turn_with_auth(&self.url, &self.auth, &session_id, prompt)?;
        self.last_turn_id = remote_turn_id(&payload).or_else(|| self.last_turn_id.clone());
        let events = remote_events_for_payload(&self.url, &self.auth, &payload)?;
        if events.is_empty() {
            return Ok(tui_lines("assistant", stable_json_dumps(&payload), false));
        }
        let events = self.filter_new_events(events);
        self.pending_events.extend(events);
        Ok(Vec::new())
    }

    fn handle_command(
        &mut self,
        command: &str,
    ) -> Result<Vec<openagent_tui::TimelineLine>, String> {
        if command == "/sessions" {
            let sessions = remote_list_sessions(&self.url, &self.auth)?;
            return Ok(tui_lines("status", remote_sessions_text(&sessions), false));
        }
        if command == "/tasks" {
            let session_id = self.ensure_session()?;
            let payload = remote_tasks_payload(&self.url, &self.auth, &session_id)?;
            return Ok(tui_lines("status", remote_tasks_text(&payload), false));
        }
        if let Some(command) = slash_terminal_command(command) {
            if command.is_empty() {
                return Ok(tui_lines("warning", "usage: /terminal <command>", true));
            }
            let text = remote_terminal_run_text(&self.url, &self.auth, command, None)?;
            return Ok(tui_lines("terminal", text, false));
        }
        if let Some(task_id) = command.strip_prefix("/task ").map(str::trim) {
            if task_id.is_empty() {
                return Ok(tui_lines("warning", "usage: /task <task_session_id>", true));
            }
            self.current_session = Some(task_id.to_string());
            return Ok(tui_lines(
                "status",
                format!("current task session: {task_id}"),
                true,
            ));
        }
        if let Some(session_id) = command.strip_prefix("/resume ").map(str::trim) {
            if session_id.is_empty() {
                return Ok(tui_lines("warning", "usage: /resume <session_id>", true));
            }
            self.current_session = Some(session_id.to_string());
            return Ok(tui_lines(
                "status",
                format!("current session: {session_id}"),
                true,
            ));
        }
        if command == "/new" {
            let session_id = remote_select_session_with_auth(
                &self.url,
                &self.auth,
                None,
                false,
                false,
                &self.workspace,
            )?;
            self.current_session = Some(session_id.clone());
            return Ok(tui_lines(
                "status",
                format!("created session: {session_id}"),
                true,
            ));
        }
        if command == "/fork" {
            let Some(base) = self.current_session.clone() else {
                return Ok(tui_lines(
                    "warning",
                    "no current session to fork; use /new or /resume <session_id>",
                    true,
                ));
            };
            let session_id = remote_select_session_with_auth(
                &self.url,
                &self.auth,
                Some(base),
                false,
                true,
                &self.workspace,
            )?;
            self.current_session = Some(session_id.clone());
            return Ok(tui_lines(
                "status",
                format!("forked session: {session_id}"),
                true,
            ));
        }
        if command == "/interrupt" || command.starts_with("/interrupt ") {
            let turn_id = command
                .strip_prefix("/interrupt ")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| self.last_turn_id.clone());
            let Some(turn_id) = turn_id else {
                return Ok(tui_lines("warning", "no turn to interrupt", true));
            };
            let payload = http_json_with_auth(
                "POST",
                &self.url,
                &format!("/api/turns/{turn_id}/interrupt"),
                &self.auth,
                None,
            )?;
            self.remember_payload_events(&payload);
            return Ok(Vec::new());
        }
        Ok(tui_lines(
            "status",
            "commands: /sessions, /tasks, /task <id>, /terminal <command>, /resume <id>, /new, /fork, /interrupt [turn_id], /exit",
            false,
        ))
    }

    fn handle_approval_response(
        &mut self,
        payload: &Value,
    ) -> Result<Vec<openagent_tui::TimelineLine>, String> {
        let turn_id = payload
            .get("turn_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "approval response missing turn_id".to_string())?;
        let request_id = payload
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "approval response missing request_id".to_string())?;
        let response = http_json_with_auth(
            "POST",
            &self.url,
            &format!("/api/turns/{turn_id}/approvals/{request_id}"),
            &self.auth,
            Some(payload.clone()),
        )?;
        self.remember_payload_events(&response);
        Ok(Vec::new())
    }

    fn handle_question_response(
        &mut self,
        payload: &Value,
    ) -> Result<Vec<openagent_tui::TimelineLine>, String> {
        let turn_id = payload
            .get("turn_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "question response missing turn_id".to_string())?;
        let request_id = payload
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "question response missing request_id".to_string())?;
        let response = http_json_with_auth(
            "POST",
            &self.url,
            &format!("/api/turns/{turn_id}/questions/{request_id}/reply"),
            &self.auth,
            Some(payload.clone()),
        )?;
        self.remember_payload_events(&response);
        Ok(Vec::new())
    }
}

fn tui_lines(
    kind: &str,
    text: impl Into<String>,
    important: bool,
) -> Vec<openagent_tui::TimelineLine> {
    let text = text.into();
    if text.trim().is_empty() {
        return Vec::new();
    }
    text.lines()
        .map(|line| openagent_tui::TimelineLine::new(kind, line.to_string(), important))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openagent_tui::TerminalEventHandler;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    #[test]
    fn remote_tasks_text_renders_nested_background_tree() {
        let payload = json!({
            "session_id": "session_root",
            "tree": [{
                "session_id": "subtask_outer",
                "title": "Outer work",
                "status": "completed",
                "subagent_type": "outer",
                "task_depth": 1,
                "background": false,
                "children": [{
                    "session_id": "subtask_inner",
                    "title": "Inner background work",
                    "status": "queued",
                    "subagent_type": "inner",
                    "task_depth": 2,
                    "background": true,
                    "children": []
                }]
            }]
        });

        let rendered = remote_tasks_text(&payload);

        assert!(rendered.contains("Remote tasks for session_root"));
        assert!(rendered.contains("subtask_outer  [completed] outer  depth=1"));
        assert!(rendered.contains("subtask_inner  [queued] inner  depth=2 background"));
        assert!(rendered.contains("Inner background work"));
    }

    #[test]
    fn remote_task_command_switches_current_session_to_task() {
        let mut handler = RemoteTerminalHandler {
            url: "http://127.0.0.1:1".to_string(),
            auth: RemoteAuth::default(),
            workspace: PathBuf::from("."),
            current_session: Some("session_root".to_string()),
            last_turn_id: None,
            last_global_event_id: 0,
            pending_events: Vec::new(),
            seen_events: BTreeSet::new(),
        };

        let lines = TerminalEventHandler::handle_command(&mut handler, "/task subtask_inner")
            .expect("task command should not hit network");

        assert_eq!(handler.current_session.as_deref(), Some("subtask_inner"));
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("current task session: subtask_inner"))
        );
    }

    #[test]
    fn remote_terminal_command_runs_through_bridge_client() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock server");
        let port = listener.local_addr().expect("mock server addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set read timeout");
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /api/terminal/run "));
            assert!(request.contains("authorization: Bearer secret"));
            assert!(request.contains("printf tui-terminal-ok"));
            let body = json!({
                "command": "printf tui-terminal-ok",
                "workspace": "/tmp/workspace",
                "cwd": "/tmp/workspace",
                "cwd_relative": ".",
                "success": true,
                "exit_code": 0,
                "stdout": "tui-terminal-ok",
                "stderr": "",
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let mut handler = RemoteTerminalHandler {
            url: format!("http://127.0.0.1:{port}"),
            auth: RemoteAuth {
                token: Some("secret".to_string()),
                username: None,
                password: None,
            },
            workspace: PathBuf::from("."),
            current_session: None,
            last_turn_id: None,
            last_global_event_id: 0,
            pending_events: Vec::new(),
            seen_events: BTreeSet::new(),
        };

        let lines =
            TerminalEventHandler::handle_command(&mut handler, "/terminal printf tui-terminal-ok")
                .expect("terminal command should run");

        assert!(
            lines
                .iter()
                .any(|line| { line.kind == "terminal" && line.text.contains("tui-terminal-ok") })
        );
        server.join().expect("mock server thread");
    }

    #[test]
    fn remote_terminal_poll_bridge_events_uses_bridge_live_stream() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock server");
        let port = listener.local_addr().expect("mock server addr").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set read timeout");
            let request = read_http_request(&mut stream);
            assert!(
                request.starts_with("GET /api/events?last_event_id=0&live_timeout_ms=250 "),
                "{request}"
            );
            assert!(request.contains("authorization: Bearer secret"));
            assert!(request.contains("accept: text/event-stream"));
            let event = json!({
                "event_id": "evt_live_delta",
                "sequence": 1,
                "global_sequence": 7,
                "method": "item/agentMessage/delta",
                "params": {"session_id": "session_live", "turn_id": "turn_live", "delta": "live delta"},
                "created_at_ms": 1,
            });
            let body = format!("event: item/agentMessage/delta\ndata: {event}\n\n: ping\n\n");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let mut handler = RemoteTerminalHandler {
            url: format!("http://127.0.0.1:{port}"),
            auth: RemoteAuth {
                token: Some("secret".to_string()),
                username: None,
                password: None,
            },
            workspace: PathBuf::from("."),
            current_session: None,
            last_turn_id: None,
            last_global_event_id: 0,
            pending_events: Vec::new(),
            seen_events: BTreeSet::new(),
        };

        let events = TerminalEventHandler::poll_bridge_events(&mut handler)
            .expect("poll should use live SSE client");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["method"], "item/agentMessage/delta");
        assert_eq!(handler.last_global_event_id, 7);
        server.join().expect("mock server thread");
    }

    fn read_http_request(stream: &mut impl Read) -> String {
        let mut raw = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).expect("read request");
            if count == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..count]);
            if request_is_complete(&raw) {
                break;
            }
        }
        String::from_utf8(raw).expect("request utf8")
    }

    fn request_is_complete(raw: &[u8]) -> bool {
        let Some(header_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let header_end = header_end + 4;
        let headers = String::from_utf8_lossy(&raw[..header_end]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default();
        raw.len() >= header_end + content_length
    }
}

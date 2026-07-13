use super::*;

pub(super) fn client_command(args: &[String]) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(client_help());
    }
    let server_url = value_for(args, &["--server-url"])
        .or_else(|| env::var("OPENAGENT_SERVER_URL").ok())
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
    let server_token = value_for(args, &["--server-token"])
        .or_else(|| env::var(DEFAULT_SERVER_TOKEN_ENV).ok())
        .or_else(|| value_for(args, &["--server-token-env"]).and_then(|name| env::var(name).ok()));
    let message = positional_args(
        args,
        &[
            "--server-url",
            "--server-token",
            "--server-token-env",
            "--workspace",
            "--dir",
            "--session",
            "-s",
            "--file",
            "-f",
            "--command",
            "--command-dir",
            "--format",
        ],
    )
    .join(" ");
    let workspace = workspace_from_args(args);
    let files = match attached_files(&workspace, &values_for(args, &["--file", "-f"])) {
        Ok(files) => files,
        Err(error) => return err_text(1, error),
    };
    let prompt = build_prompt_with_files(&message, &files);
    if prompt.trim().is_empty() {
        return err_text(2, "openagent client requires a prompt");
    }
    let session_id = match remote_select_session(
        &server_url,
        server_token.as_deref(),
        value_for(args, &["--session", "-s"]),
        has_flag(args, &["--continue", "-c"]),
        &workspace,
    ) {
        Ok(session_id) => session_id,
        Err(error) => return err_text(1, error),
    };
    let payload =
        match remote_start_turn(&server_url, server_token.as_deref(), &session_id, &prompt) {
            Ok(payload) => payload,
            Err(error) => return err_text(1, error),
        };
    if value_for(args, &["--format"]).as_deref() == Some("json") {
        if let Some(events) = payload.get("events").and_then(Value::as_array) {
            return ok_text(
                events
                    .iter()
                    .map(stable_json_dumps)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        return CliRunResult::ok_json(&payload);
    }
    if let Some(events) = payload.get("events").and_then(Value::as_array) {
        ok_text(text_from_bridge_events(events))
    } else {
        ok_text(stable_json_dumps(&payload))
    }
}

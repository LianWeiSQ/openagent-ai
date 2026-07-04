use super::*;

mod agent_loop;
mod mcp_runtime;
mod profile;
mod provider;
mod tool;

use agent_loop::{AgentLoopRequest, pending_resume_from_session, run_agent_loop};
use mcp_runtime::{McpRuntime, execute_mcp_tool, load_mcp_runtime};
pub(crate) use openagent_mcp::discover_mcp_server_tools;
use profile::{
    RunAgentProfile, agent_tool_options, bind_agent_profile_system_prompt, child_task_depth,
    child_task_lineage, filter_tools_for_agent, is_subagent_mode, load_agent_profile_from_args,
    max_subagent_depth_cli, parent_task_lineage, permission_manager_for_agent,
    permission_ruleset_for_profile, permission_ruleset_from_args, provider_and_model_for_subagent,
    provider_and_model_from_args, subagent_task_governance_error, task_root_session_id,
    task_subagent_descriptors,
};
pub(crate) use profile::{
    agent_profile_public_value, available_agent_profiles, load_agent_profile_by_name,
};
use provider::{add_usage, call_provider_for_run};
pub(crate) use tool::split_answer_items;

pub(super) fn run_prompt_command(args: &[String]) -> CliRunResult {
    run_prompt_command_with_events(args, None)
}

pub(super) fn run_prompt_command_with_events(
    args: &[String],
    mut event_sink: Option<&mut dyn FnMut(&Value)>,
) -> CliRunResult {
    if args.iter().any(|arg| is_help_flag(arg)) {
        return ok_text(run_help());
    }
    let format = value_for(args, &["--format"]).unwrap_or_else(|| "text".to_string());
    if let Some(url) = value_for(args, &["--attach"]) {
        return run_attached_command(args, &url, event_sink);
    }
    let workspace = workspace_from_args(args);
    let agent_profile = match load_agent_profile_from_args(args, &workspace) {
        Ok(profile) => profile,
        Err(error) => return err_text(2, error),
    };
    let (provider, model_id) = provider_and_model_from_args(args, agent_profile.as_ref());
    if !has_flag(args, &["--skip-doctor"])
        && !doctor_payload_from_args(&provider, args)["healthy"]
            .as_bool()
            .unwrap_or(false)
    {
        return err_text(
            2,
            "Gateway check failed. Start your local OpenAI-compatible service, or rerun with --skip-doctor.",
        );
    }
    let permission_ruleset = match permission_ruleset_from_args(args, agent_profile.as_ref()) {
        Ok(ruleset) => ruleset,
        Err(error) => return err_text(2, error),
    };
    let skip_permissions = has_flag(args, &["--dangerously-skip-permissions"]);
    let agent_name = agent_profile
        .as_ref()
        .map(|profile| profile.id.clone())
        .or_else(|| value_for(args, &["--agent"]))
        .unwrap_or_else(|| "default".to_string());
    let max_steps = value_for(args, &["--max-steps"])
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(UNBOUNDED_MAX_STEPS);
    let run_selection = match prepare_run_session(args, &workspace, &provider, &model_id) {
        Ok(selection) => selection,
        Err(error) => return err_text(1, error),
    };
    let forked = run_selection.forked;
    let mut session = run_selection.session;
    let store = run_selection.store;
    let pending_resume = pending_resume_from_session(&session);
    let (message, prompt, has_user_prompt) = match build_run_prompt(
        args,
        &workspace,
        agent_profile.as_ref(),
        pending_resume.is_some(),
    ) {
        Ok(prompt) => prompt,
        Err(error) => {
            return err_text(
                if error.contains("requires a prompt") {
                    2
                } else {
                    1
                },
                error,
            );
        }
    };
    let run_id = new_cli_id("run");
    let trace_id = new_cli_id("trace");
    session.status = SessionStatus::Running;
    session
        .metadata
        .insert("agent".to_string(), json!(agent_name.clone()));
    session
        .metadata
        .insert("provider".to_string(), json!(provider.clone()));
    session
        .metadata
        .insert("model".to_string(), json!(model_id.clone()));
    if let Some(title) = title_from_args(args, &message) {
        session.metadata.insert("title".to_string(), json!(title));
    }
    if let Some(profile) = agent_profile.as_ref() {
        session.metadata.insert(
            "agent_profile".to_string(),
            agent_profile_public_value(profile),
        );
    }
    if let Some(variant) = value_for(args, &["--variant"]) {
        session
            .metadata
            .insert("variant".to_string(), json!(variant));
    }
    session.metadata.insert(
        "thinking".to_string(),
        json!(has_flag(args, &["--thinking"])),
    );
    session.metadata.insert(
        "dangerously_skip_permissions".to_string(),
        json!(skip_permissions),
    );
    session
        .metadata
        .insert("permission".to_string(), json!(permission_ruleset.as_str()));
    if let Err(error) = store.start_run(
        &mut session,
        StartRunOptions {
            run_id: run_id.clone(),
            trace_id,
            agent_name,
            model_id: Some(model_id.clone()),
            provider_id: Some(provider.clone()),
            permission: if skip_permissions {
                format!("auto_allow:{}", permission_ruleset.as_str())
            } else {
                permission_ruleset.as_str().to_string()
            },
            max_steps,
            started_at_ms: None,
        },
    ) {
        return err_text(1, format!("failed to start session run: {error}"));
    }
    if let Err(error) =
        bind_agent_profile_system_prompt(&mut session, &store, &run_id, agent_profile.as_ref())
    {
        return err_text(1, error);
    }
    if has_user_prompt {
        let user_message = chat_message(Role::User, prompt.clone());
        let user_index = session.messages.len() as u64;
        session.add(user_message.clone());
        if let Err(error) = store.append_message(&session, &user_message, &run_id, user_index) {
            return err_text(1, format!("failed to record user message: {error}"));
        }
    }
    let loop_result = run_agent_loop(
        AgentLoopRequest {
            args,
            workspace: &workspace,
            provider: &provider,
            model_id: &model_id,
            session: &mut session,
            store: &store,
            run_id: &run_id,
            max_steps,
            prompt: &prompt,
            agent_profile: agent_profile.as_ref(),
            permission_ruleset,
            skip_permissions,
        },
        &mut event_sink,
    );
    let loop_result = match loop_result {
        Ok(result) => result,
        Err(error) => {
            session.status = if error.paused {
                SessionStatus::Paused
            } else {
                SessionStatus::Stop
            };
            let finish_reason = error.finish_reason.as_deref().unwrap_or(if error.paused {
                "paused"
            } else {
                "error"
            });
            let _ = store.finish_run(
                &session,
                &run_id,
                "failed",
                error.steps.max(1),
                Some(finish_reason),
                Some(&error.message),
            );
            if format == "json" && !error.events.is_empty() {
                return err_json_events(
                    error.events,
                    error.message,
                    if error.paused { "paused" } else { "failed" },
                    &mut event_sink,
                );
            }
            return err_text(1, error.message);
        }
    };
    let answer = loop_result.answer.clone();
    session.status = SessionStatus::Idle;
    let _ = store.record_event(
        &session.id,
        &run_id,
        "model.usage",
        SessionEventOptions {
            kind: "model".to_string(),
            attributes: BTreeMap::from([
                (
                    "input_tokens".to_string(),
                    json!(loop_result.usage.input_tokens),
                ),
                (
                    "output_tokens".to_string(),
                    json!(loop_result.usage.output_tokens),
                ),
                ("cost".to_string(), json!(loop_result.usage.cost)),
                ("source".to_string(), json!(loop_result.source.clone())),
                ("tool_calls".to_string(), json!(loop_result.tool_calls)),
            ]),
            ..SessionEventOptions::default()
        },
    );
    if let Err(error) = store.finish_run(
        &session,
        &run_id,
        "completed",
        loop_result.steps.max(1),
        Some(&loop_result.finish_reason),
        None,
    ) {
        return err_text(1, format!("failed to finish session run: {error}"));
    }
    let share = if has_flag(args, &["--share"]) {
        match share_session(&store, &session.id, false) {
            Ok(value) => Some(value),
            Err(error) => return err_text(1, error),
        }
    } else {
        None
    };
    if format == "json" {
        let mut completed = json!({
            "method": "turn/completed",
            "params": {
                "status": "completed",
                "final_answer": answer.clone(),
                "session_id": session.id.clone(),
                "run_id": run_id.clone(),
                "provider": provider.clone(),
                "source": loop_result.source,
                "forked": forked,
                "steps": loop_result.steps,
                "tool_calls": loop_result.tool_calls,
            }
        });
        if let Some(share) = share {
            completed["params"]["share"] = share;
        }
        if let Some(emit) = event_sink {
            emit(&completed);
            return CliRunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            };
        }
        let mut events = loop_result.events;
        events.push(completed);
        return ok_text(
            events
                .iter()
                .map(stable_json_dumps)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    let mut text = answer;
    if let Some(share) = share
        && let Some(url) = share.get("url").and_then(Value::as_str)
    {
        text.push_str(&format!("\n\nShared session: {url}"));
    }
    ok_text(text)
}

fn run_attached_command(
    args: &[String],
    server_url: &str,
    event_sink: Option<&mut dyn FnMut(&Value)>,
) -> CliRunResult {
    let format = value_for(args, &["--format"]).unwrap_or_else(|| "text".to_string());
    let auth = remote_auth_from_args(args);
    let workspace = workspace_from_args(args);
    let (_, prompt, _) = match build_run_prompt(args, &workspace, None, false) {
        Ok(prompt) => prompt,
        Err(error) => {
            return err_text(
                if error.contains("requires a prompt") {
                    2
                } else {
                    1
                },
                error,
            );
        }
    };
    let session_id = match remote_select_session_with_auth(
        server_url,
        &auth,
        value_for(args, &["--session", "-s"]),
        has_flag(args, &["--continue", "-c"]),
        has_flag(args, &["--fork"]),
        &workspace,
    ) {
        Ok(session_id) => session_id,
        Err(error) => return err_text(1, error),
    };
    let payload = match remote_start_turn_with_auth(server_url, &auth, &session_id, &prompt) {
        Ok(payload) => payload,
        Err(error) => return err_text(1, error),
    };
    let events = match remote_events_for_payload(server_url, &auth, &payload) {
        Ok(events) => events,
        Err(error) => return err_text(1, error),
    };
    if format == "json" {
        if !events.is_empty() {
            if let Some(emit) = event_sink {
                for event in &events {
                    emit(event);
                }
                return CliRunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                };
            }
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
    if !events.is_empty() {
        ok_text(text_from_app_events(&events))
    } else {
        ok_text(stable_json_dumps(&payload))
    }
}

#[derive(Debug)]
struct RunSessionSelection {
    store: FileSessionStore,
    session: Session,
    forked: bool,
}

fn read_piped_stdin() -> String {
    if io::stdin().is_terminal() {
        return String::new();
    }
    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);
    input
}

fn build_run_prompt(
    args: &[String],
    workspace: &Path,
    _agent_profile: Option<&RunAgentProfile>,
    allow_empty_resume: bool,
) -> Result<(String, String, bool), String> {
    let message_args = positional_args(args, RUN_POSITIONAL_VALUE_FLAGS);
    let message = message_args.join(" ");
    let stdin_text = read_piped_stdin();
    let message = if message.trim().is_empty() {
        stdin_text.trim().to_string()
    } else if stdin_text.trim().is_empty() {
        message
    } else {
        format!("{}\n{}", message.trim(), stdin_text.trim())
    };
    if message.trim().is_empty() {
        if allow_empty_resume {
            return Ok((String::new(), String::new(), false));
        }
        return Err("openagent run requires a prompt or piped stdin".to_string());
    }
    let files = attached_files(workspace, &values_for(args, &["--file", "-f"]))?;
    let prompt = if let Some(command_name) = value_for(args, &["--command"]) {
        let mut command_args = message_args;
        if !stdin_text.trim().is_empty() {
            command_args.push(stdin_text.trim().to_string());
        }
        let command = discover_custom_commands(args)
            .into_iter()
            .find(|item| item.name == command_name)
            .ok_or_else(|| format!("Command not found: {command_name}"))?;
        let rendered = render_custom_template(&command.template, &command_args, workspace);
        build_prompt_with_files(&rendered, &files)
    } else {
        build_prompt_with_files(&message, &files)
    };
    Ok((message, prompt, true))
}

fn err_json_events(
    mut events: Vec<Value>,
    message: String,
    status: &str,
    event_sink: &mut Option<&mut dyn FnMut(&Value)>,
) -> CliRunResult {
    let failed = json!({
        "method": "turn/completed",
        "params": {
            "status": status,
            "error": message,
        }
    });
    if let Some(emit) = event_sink.as_deref_mut() {
        emit(&failed);
        return CliRunResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
        };
    }
    events.push(failed);
    CliRunResult {
        exit_code: 1,
        stdout: ensure_trailing_newline(
            events
                .iter()
                .map(stable_json_dumps)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        stderr: String::new(),
    }
}

fn title_from_args(args: &[String], message: &str) -> Option<String> {
    let title = value_for(args, &["--title"])?;
    if title.is_empty() {
        let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
        return Some(if compact.chars().count() > 50 {
            format!("{}...", compact.chars().take(50).collect::<String>())
        } else {
            compact
        });
    }
    Some(title)
}

fn prepare_run_session(
    args: &[String],
    workspace: &Path,
    provider: &str,
    model_id: &str,
) -> Result<RunSessionSelection, String> {
    let root = session_root_from_args(args);
    let store = FileSessionStore::new(root.clone());
    let explicit = value_for(args, &["--session", "-s"]);
    let continue_last = has_flag(args, &["--continue", "-c"]);
    let fork = has_flag(args, &["--fork"]);
    if fork && explicit.is_none() && !continue_last {
        return Err("--fork requires --continue or --session".to_string());
    }
    let base_id = explicit.or_else(|| continue_last.then(|| latest_session_id(&root)).flatten());
    let mut session = if let Some(session_id) = base_id {
        if !valid_session_id(&session_id) {
            return Err("Invalid session id".to_string());
        }
        store
            .load_session(&session_id)
            .map_err(|error| error.to_string())?
    } else {
        Session::new(new_cli_id("session"), workspace)
    };
    let forked = fork;
    if forked {
        let source_session_id = session.id.clone();
        session = store
            .fork_session(&source_session_id, &new_cli_id("session"), workspace, None)
            .map_err(|error| format!("failed to fork session {source_session_id}: {error}"))?;
    }
    session
        .metadata
        .insert("provider".to_string(), json!(provider));
    session
        .metadata
        .insert("model".to_string(), json!(model_id));
    Ok(RunSessionSelection {
        store,
        session,
        forked,
    })
}

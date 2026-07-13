use super::tool::{
    add_approval_always_pattern, approval_always_patterns, approval_payload_for_tool_call,
    assistant_message_for_provider_step, configured_question_answers, execute_agent_tool,
};
use super::*;
use openagent_tools::{
    SessionRunnerFacade, TASK_TOOL_ID, TaskSubagentRoute,
    benchmark_mode_value_allows_shell_command, prepare_isolated_workspace,
    question_answers_from_json, select_task_subagent_for_prompt, value_to_answer_string,
};

#[derive(Debug)]
pub(super) struct AgentLoopOutcome {
    pub(super) answer: String,
    pub(super) usage: Usage,
    pub(super) source: String,
    pub(super) events: Vec<Value>,
    pub(super) steps: u64,
    pub(super) tool_calls: u64,
    pub(super) finish_reason: String,
}

#[derive(Debug)]
pub(super) struct AgentLoopError {
    pub(super) message: String,
    pub(super) events: Vec<Value>,
    pub(super) steps: u64,
    pub(super) finish_reason: Option<String>,
    pub(super) paused: bool,
}

#[derive(Clone, Debug)]
pub(super) struct PendingResume {
    pub(super) kind: String,
    pub(super) request_id: String,
    pub(super) call: ToolCall,
    pub(super) response: Value,
    pub(super) step: u64,
}

pub(super) struct AgentLoopRequest<'a> {
    pub(super) args: &'a [String],
    pub(super) workspace: &'a Path,
    pub(super) provider: &'a str,
    pub(super) model_id: &'a str,
    pub(super) session: &'a mut Session,
    pub(super) store: &'a FileSessionStore,
    pub(super) run_id: &'a str,
    pub(super) max_steps: u64,
    pub(super) prompt: &'a str,
    pub(super) agent_profile: Option<&'a RunAgentProfile>,
    pub(super) permission_ruleset: PermissionRuleset,
    pub(super) skip_permissions: bool,
}

pub(super) fn run_agent_loop(
    request: AgentLoopRequest<'_>,
    event_sink: &mut Option<&mut dyn FnMut(&Value)>,
) -> Result<AgentLoopOutcome, AgentLoopError> {
    let AgentLoopRequest {
        args,
        workspace,
        provider,
        model_id,
        session,
        store,
        run_id,
        max_steps,
        prompt,
        agent_profile,
        permission_ruleset,
        skip_permissions,
    } = request;
    let mut toolkit = Toolkit::with_builtins();
    let mcp_runtime = load_mcp_runtime(args, &mut toolkit).map_err(|message| AgentLoopError {
        message,
        events: Vec::new(),
        steps: 0,
        finish_reason: Some("mcp_discovery_error".to_string()),
        paused: false,
    })?;
    let benchmark_mode_disables_subagents = benchmark_mode_disables_cli_subagents();
    let subagent_descriptors = if benchmark_mode_disables_subagents {
        Vec::new()
    } else {
        task_subagent_descriptors(args, agent_profile, Some(session))
    };
    if !benchmark_mode_disables_subagents {
        register_task_tool(&mut toolkit.registry, &subagent_descriptors);
    }
    let tools = filter_tools_for_agent(toolkit.get_all_tools("local"), agent_profile);
    let mut runner_facade = SessionRunnerFacade::new(workspace, session.id.clone())
        .with_agent_options(agent_tool_options(agent_profile))
        .with_permission_manager(permission_manager_for_agent(
            permission_ruleset.clone(),
            agent_profile,
        ))
        .with_dangerously_skip_permissions(skip_permissions);
    if let Some(answers) = configured_question_answers(args) {
        runner_facade = runner_facade.with_question_answers(answers);
    }
    let mut ctx = runner_facade.tool_context();
    if let Some(runtime) = mcp_runtime.as_ref() {
        let _ = store.record_event(
            &session.id,
            run_id,
            "mcp.discovery",
            SessionEventOptions {
                kind: "mcp".to_string(),
                attributes: BTreeMap::from([(
                    "snapshot".to_string(),
                    sanitize_mcp_observation_value(&runtime.snapshot),
                )]),
                ..SessionEventOptions::default()
            },
        );
    }

    let mut answer = String::new();
    let mut events = Vec::new();
    let mut total_usage = Usage::default();
    let mut total_tool_calls = 0_u64;
    let mut first_delta = true;
    let mut approval_always = approval_always_patterns(session);

    if let Some(pending) = pending_resume_from_session(session) {
        total_tool_calls += 1;
        let mut resume_context = PendingResumeContext {
            args,
            workspace,
            provider,
            model_id,
            toolkit: &toolkit,
            mcp_runtime: mcp_runtime.as_ref(),
            ctx: &mut ctx,
            session,
            store,
            run_id,
            max_steps,
            permission_ruleset: permission_ruleset.clone(),
            skip_permissions,
            events: &mut events,
            event_sink,
        };
        process_pending_resume(pending, &mut resume_context).map_err(|message| AgentLoopError {
            message,
            events: events.clone(),
            steps: 0,
            finish_reason: Some("resume_error".to_string()),
            paused: false,
        })?;
        approval_always = approval_always_patterns(session);
    }

    if let Some(route) = direct_subagent_route(prompt, &subagent_descriptors) {
        let tool_call = route.tool_call.clone();
        let route_source = route.source;
        let route_metadata = route.metadata.clone();
        total_tool_calls += 1;
        let assistant_index = session.messages.len() as u64;
        let assistant_message_id = cli_message_id(assistant_index);
        let step_start_checkpoint = create_step_checkpoint(
            store,
            &session.id,
            run_id,
            workspace,
            1,
            "step_start",
            &assistant_message_id,
        );
        record_step_started(
            store,
            &session.id,
            run_id,
            1,
            step_start_checkpoint.as_deref(),
        );
        emit_run_event(
            &mut events,
            runner_facade.tool_call_started_event(
                run_id,
                1,
                &tool_call,
                None,
                BTreeMap::from([
                    ("manual".to_string(), json!(route.manual)),
                    ("auto".to_string(), json!(route.auto)),
                    ("auto_route".to_string(), route_metadata.clone()),
                ]),
            ),
            event_sink,
        );
        let mut assistant =
            assistant_message_for_provider_step(String::new(), &[tool_call.clone()]);
        assistant.metadata.insert(
            "message_id".to_string(),
            json!(assistant_message_id.clone()),
        );
        if let Some(checkpoint_id) = step_start_checkpoint.as_deref() {
            assistant
                .metadata
                .insert("snapshot_start".to_string(), json!(checkpoint_id));
        }
        assistant.metadata.insert("step".to_string(), json!(1));
        session.add(assistant.clone());
        store
            .append_message(session, &assistant, run_id, assistant_index)
            .map_err(|error| AgentLoopError {
                message: format!("failed to record {route_source} call: {error}"),
                events: events.clone(),
                steps: 1,
                finish_reason: Some("store_error".to_string()),
                paused: false,
            })?;
        let tool_result = execute_loop_tool_call(
            &toolkit,
            mcp_runtime.as_ref(),
            &tool_call,
            &mut ctx,
            TaskExecutionContext {
                args,
                workspace,
                provider,
                model_id,
                session,
                store,
                run_id,
                max_steps,
                permission_ruleset: permission_ruleset.clone(),
                skip_permissions,
            },
        );
        let failed = tool_result.error.is_some();
        emit_run_event(
            &mut events,
            runner_facade.tool_call_finished_event(
                run_id,
                1,
                &tool_call,
                &tool_result,
                None,
                BTreeMap::from([
                    ("manual".to_string(), json!(route.manual)),
                    ("auto".to_string(), json!(route.auto)),
                    ("auto_route".to_string(), route_metadata.clone()),
                ]),
            ),
            event_sink,
        );
        let tool_message = runner_facade.tool_result_message(
            1,
            &tool_call,
            &tool_result,
            Some(&assistant_message_id),
            Some(new_cli_id("msg")),
        );
        let tool_index = session.messages.len() as u64;
        session.add(tool_message.clone());
        store
            .append_message(session, &tool_message, run_id, tool_index)
            .map_err(|error| AgentLoopError {
                message: format!("failed to record {route_source} result: {error}"),
                events: events.clone(),
                steps: 1,
                finish_reason: Some("store_error".to_string()),
                paused: false,
            })?;
        let final_answer = tool_result
            .error
            .clone()
            .unwrap_or_else(|| tool_result.output.clone());
        finalize_step_checkpoint(
            store,
            &session.id,
            run_id,
            workspace,
            1,
            &assistant_message_id,
            step_start_checkpoint.as_deref(),
        );
        record_step_finished(
            store,
            &session.id,
            run_id,
            1,
            if failed { "tool_error" } else { "stop" },
            total_tool_calls,
            &total_usage,
        );
        return Ok(AgentLoopOutcome {
            answer: final_answer,
            usage: total_usage,
            source: route_source.to_string(),
            events,
            steps: 1,
            tool_calls: total_tool_calls,
            finish_reason: if failed { "tool_error" } else { "stop" }.to_string(),
        });
    }

    for step in 1..=max_steps {
        let mut streamed_events = Vec::new();
        let assistant_index = session.messages.len() as u64;
        let assistant_message_id = cli_message_id(assistant_index);
        record_step_started(store, &session.id, run_id, step, None);
        let provider_messages =
            super::profile::materialized_provider_messages_for_agent(session, agent_profile);
        let mut on_provider_stream = |event: &ProviderStreamEvent| {
            if let ProviderStreamEvent::TextDelta { text } = event
                && !text.is_empty()
            {
                let mut params = json!({
                    "delta": text,
                    "session_id": session.id.clone(),
                    "run_id": run_id,
                    "step": step,
                });
                if first_delta {
                    params["prompt"] = json!(prompt);
                    first_delta = false;
                }
                emit_run_event(
                    &mut streamed_events,
                    json!({"method": "item/agentMessage/delta", "params": params}),
                    event_sink,
                );
            }
        };
        let provider_result = call_provider_for_run(
            args,
            provider,
            model_id,
            &provider_messages,
            &tools,
            Some(&mut on_provider_stream),
            agent_profile,
        )
        .map_err(|message| AgentLoopError {
            message,
            events: events.clone(),
            steps: step,
            finish_reason: Some("provider_error".to_string()),
            paused: false,
        })?;
        let streamed_text = !streamed_events.is_empty();
        events.extend(streamed_events);
        let source = provider_result.source.clone();
        add_usage(&mut total_usage, &provider_result.usage);
        let step_text = provider_result.answer.clone();
        if !step_text.is_empty() {
            answer.push_str(&step_text);
            if !streamed_text {
                let mut params = json!({
                    "delta": step_text,
                    "session_id": session.id.clone(),
                    "run_id": run_id,
                    "step": step,
                });
                if first_delta {
                    params["prompt"] = json!(prompt);
                    first_delta = false;
                }
                emit_run_event(
                    &mut events,
                    json!({"method": "item/agentMessage/delta", "params": params}),
                    event_sink,
                );
            }
            store
                .append_part(
                    &session.id,
                    run_id,
                    "text",
                    SessionPartOptions {
                        attributes: BTreeMap::from([
                            ("role".to_string(), json!("assistant")),
                            ("chars".to_string(), json!(step_text.chars().count())),
                        ]),
                        step_index: Some(step),
                        ..SessionPartOptions::default()
                    },
                )
                .map_err(|error| AgentLoopError {
                    message: format!("failed to record assistant text part: {error}"),
                    events: events.clone(),
                    steps: step,
                    finish_reason: Some("store_error".to_string()),
                    paused: false,
                })?;
        }

        let mut assistant_message =
            assistant_message_for_provider_step(step_text, &provider_result.tool_calls);
        assistant_message.metadata.insert(
            "message_id".to_string(),
            json!(assistant_message_id.clone()),
        );
        assistant_message
            .metadata
            .insert("step".to_string(), json!(step));
        session.add(assistant_message.clone());
        store
            .append_message(session, &assistant_message, run_id, assistant_index)
            .map_err(|error| AgentLoopError {
                message: format!("failed to record assistant message: {error}"),
                events: events.clone(),
                steps: step,
                finish_reason: Some("store_error".to_string()),
                paused: false,
            })?;

        let step_outcome = SessionRunnerFacade::provider_step_outcome(
            provider_result.tool_calls.len() as u64,
            &provider_result.finish_reason,
        );
        if step_outcome.is_complete() {
            record_step_finished(
                store,
                &session.id,
                run_id,
                step,
                &step_outcome.finish_reason,
                step_outcome.tool_call_count,
                &provider_result.usage,
            );
            return Ok(AgentLoopOutcome {
                answer,
                usage: total_usage,
                source,
                events,
                steps: step,
                tool_calls: total_tool_calls,
                finish_reason: step_outcome.finish_reason,
            });
        }
        debug_assert!(step_outcome.continues_with_tools());

        let step_start_checkpoint = create_step_checkpoint(
            store,
            &session.id,
            run_id,
            workspace,
            step,
            "step_start",
            &assistant_message_id,
        );
        for tool_call in provider_result.tool_calls {
            total_tool_calls += 1;
            emit_run_event(
                &mut events,
                runner_facade.tool_call_started_event(
                    run_id,
                    step,
                    &tool_call,
                    None,
                    BTreeMap::new(),
                ),
                event_sink,
            );
            let _ = store.record_event(
                &session.id,
                run_id,
                "tool.call.started",
                SessionEventOptions {
                    kind: "tool".to_string(),
                    attributes: BTreeMap::from([
                        ("call_id".to_string(), json!(tool_call.call_id.clone())),
                        ("name".to_string(), json!(tool_call.name.clone())),
                        ("input".to_string(), tool_call.input.clone()),
                        ("step".to_string(), json!(step)),
                    ]),
                    ..SessionEventOptions::default()
                },
            );

            if tool_call.name == "question" && ctx.question_answers.is_none() {
                let message =
                    "question tool requires an answer; rerun with --answer or OPENAGENT_QUESTION_ANSWERS".to_string();
                emit_run_event(
                    &mut events,
                    json!({
                        "method": "turn/question_requested",
                        "params": {
                            "session_id": session.id.clone(),
                            "run_id": run_id,
                            "step": step,
                            "call_id": tool_call.call_id.clone(),
                            "questions": tool_call.input.get("questions").cloned().unwrap_or_else(|| json!([])),
                        }
                    }),
                    event_sink,
                );
                let _ = store.record_event(
                    &session.id,
                    run_id,
                    "question.requested",
                    SessionEventOptions {
                        kind: "question".to_string(),
                        attributes: BTreeMap::from([
                            ("call_id".to_string(), json!(tool_call.call_id.clone())),
                            (
                                "questions".to_string(),
                                tool_call
                                    .input
                                    .get("questions")
                                    .cloned()
                                    .unwrap_or_else(|| json!([])),
                            ),
                        ]),
                        ..SessionEventOptions::default()
                    },
                );
                let _ = store.append_part(
                    &session.id,
                    run_id,
                    "question",
                    SessionPartOptions {
                        message_id: Some(assistant_message_id.clone()),
                        content: Some(json!({
                            "call_id": tool_call.call_id.clone(),
                            "name": tool_call.name.clone(),
                            "questions": tool_call.input.get("questions").cloned().unwrap_or_else(|| json!([])),
                            "status": "pending",
                        })),
                        attributes: BTreeMap::from([
                            ("call_id".to_string(), json!(tool_call.call_id.clone())),
                            ("name".to_string(), json!(tool_call.name.clone())),
                        ]),
                        step_index: Some(step),
                        status: "pending".to_string(),
                        ..SessionPartOptions::default()
                    },
                );
                session.metadata.insert(
                    "pending_question".to_string(),
                    json!({
                        "request_id": format!("question_{}", tool_call.call_id),
                        "session_id": session.id.clone(),
                        "turn_id": run_id,
                        "run_id": run_id,
                        "step": step,
                        "call_id": tool_call.call_id.clone(),
                        "tool_name": tool_call.name.clone(),
                        "tool_input": tool_call.input.clone(),
                        "assistant_message_id": assistant_message_id.clone(),
                        "questions": tool_call.input.get("questions").cloned().unwrap_or_else(|| json!([])),
                        "created_at_ms": now_ms_cli(),
                    }),
                );
                session.metadata.remove("pending_question_response");
                let _ = store.save_state(session, Some(run_id));
                return Err(AgentLoopError {
                    message,
                    events,
                    steps: step,
                    finish_reason: Some("question_required".to_string()),
                    paused: true,
                });
            }

            let mut tool_result = execute_loop_tool_call(
                &toolkit,
                mcp_runtime.as_ref(),
                &tool_call,
                &mut ctx,
                TaskExecutionContext {
                    args,
                    workspace,
                    provider,
                    model_id,
                    session,
                    store,
                    run_id,
                    max_steps,
                    permission_ruleset: permission_ruleset.clone(),
                    skip_permissions,
                },
            );
            if tool_result
                .metadata
                .get("requires_approval")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let pattern = tool_result
                    .metadata
                    .get("permission_pattern")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if approval_always.iter().any(|item| item == &pattern) {
                    let previous = ctx.dangerously_skip_permissions;
                    ctx.dangerously_skip_permissions = true;
                    tool_result = execute_loop_tool_call(
                        &toolkit,
                        mcp_runtime.as_ref(),
                        &tool_call,
                        &mut ctx,
                        TaskExecutionContext {
                            args,
                            workspace,
                            provider,
                            model_id,
                            session,
                            store,
                            run_id,
                            max_steps,
                            permission_ruleset: permission_ruleset.clone(),
                            skip_permissions,
                        },
                    );
                    ctx.dangerously_skip_permissions = previous;
                }
            }
            if tool_result
                .metadata
                .get("requires_approval")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let message = format!(
                    "approval required for tool {} (call {})",
                    tool_call.name, tool_call.call_id
                );
                let mut approval = approval_payload_for_tool_call(
                    session,
                    run_id,
                    step,
                    &tool_call,
                    &tool_result.metadata,
                );
                if let Some(object) = approval.as_object_mut() {
                    object.insert(
                        "assistant_message_id".to_string(),
                        json!(assistant_message_id.clone()),
                    );
                }
                emit_run_event(
                    &mut events,
                    json!({
                        "method": "turn/approval_requested",
                        "params": {
                            "session_id": session.id.clone(),
                            "run_id": run_id,
                            "step": step,
                            "approval": approval,
                        }
                    }),
                    event_sink,
                );
                let _ = store.record_event(
                    &session.id,
                    run_id,
                    "approval.requested",
                    SessionEventOptions {
                        kind: "approval".to_string(),
                        attributes: BTreeMap::from([
                            ("call_id".to_string(), json!(tool_call.call_id.clone())),
                            ("name".to_string(), json!(tool_call.name.clone())),
                            (
                                "reason".to_string(),
                                json!(
                                    tool_result
                                        .metadata
                                        .get("error_kind")
                                        .and_then(Value::as_str)
                                        .unwrap_or("permission_required")
                                ),
                            ),
                            ("metadata".to_string(), json!(tool_result.metadata)),
                        ]),
                        ..SessionEventOptions::default()
                    },
                );
                let _ = store.append_part(
                    &session.id,
                    run_id,
                    "approval",
                    SessionPartOptions {
                        message_id: Some(assistant_message_id.clone()),
                        content: Some(json!({
                            "call_id": tool_call.call_id.clone(),
                            "name": tool_call.name.clone(),
                            "approval": approval.clone(),
                            "status": "pending",
                        })),
                        attributes: BTreeMap::from([
                            ("call_id".to_string(), json!(tool_call.call_id.clone())),
                            ("name".to_string(), json!(tool_call.name.clone())),
                        ]),
                        step_index: Some(step),
                        status: "pending".to_string(),
                        ..SessionPartOptions::default()
                    },
                );
                session
                    .metadata
                    .insert("pending_approval".to_string(), approval.clone());
                session.metadata.remove("pending_approval_response");
                let _ = store.save_state(session, Some(run_id));
                return Err(AgentLoopError {
                    message,
                    events,
                    steps: step,
                    finish_reason: Some("approval_required".to_string()),
                    paused: true,
                });
            }
            emit_run_event(
                &mut events,
                runner_facade.tool_call_finished_event(
                    run_id,
                    step,
                    &tool_call,
                    &tool_result,
                    None,
                    BTreeMap::new(),
                ),
                event_sink,
            );
            let settlement = runner_facade.tool_result_settlement(
                step,
                &tool_call,
                &tool_result,
                Some(&assistant_message_id),
                Some(new_cli_id("msg")),
            );
            for intent in &settlement.event_intents {
                let _ = store.record_event(
                    &session.id,
                    run_id,
                    &intent.event_name,
                    SessionEventOptions {
                        kind: intent.kind.clone(),
                        status: intent.status.clone(),
                        attributes: intent.attributes.clone(),
                        ..SessionEventOptions::default()
                    },
                );
            }
            let part_intent = &settlement.part_intent;
            let _ = store.append_part(
                &session.id,
                run_id,
                &part_intent.part_type,
                SessionPartOptions {
                    attributes: part_intent.attributes.clone(),
                    step_index: part_intent.step_index,
                    status: part_intent.status.clone(),
                    ..SessionPartOptions::default()
                },
            );

            let tool_message = settlement.message;
            let tool_index = session.messages.len() as u64;
            session.add(tool_message.clone());
            store
                .append_message(session, &tool_message, run_id, tool_index)
                .map_err(|error| AgentLoopError {
                    message: format!("failed to record tool message: {error}"),
                    events: events.clone(),
                    steps: step,
                    finish_reason: Some("store_error".to_string()),
                    paused: false,
                })?;
        }

        finalize_step_checkpoint(
            store,
            &session.id,
            run_id,
            workspace,
            step,
            &assistant_message_id,
            step_start_checkpoint.as_deref(),
        );
        record_step_finished(
            store,
            &session.id,
            run_id,
            step,
            "tool_call",
            total_tool_calls,
            &provider_result.usage,
        );
    }

    Err(AgentLoopError {
        message: format!("agent loop exceeded max steps ({max_steps})"),
        events,
        steps: max_steps,
        finish_reason: Some("max_steps".to_string()),
        paused: false,
    })
}

fn manual_subagent_tool_call(prompt: &str) -> Option<ToolCall> {
    let trimmed = prompt.trim_start();
    let rest = trimmed.strip_prefix('@')?;
    let (subagent_type, task_prompt) = rest.split_once(char::is_whitespace)?;
    let subagent_type = subagent_type.trim();
    let task_prompt = task_prompt.trim();
    if subagent_type.is_empty()
        || task_prompt.is_empty()
        || !subagent_type
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return None;
    }
    Some(ToolCall {
        name: TASK_TOOL_ID.to_string(),
        input: json!({
            "description": format!("@{subagent_type}"),
            "prompt": task_prompt,
            "subagent_type": subagent_type,
        }),
        call_id: format!("manual_task_{subagent_type}"),
    })
}

struct DirectSubagentRoute {
    tool_call: ToolCall,
    source: &'static str,
    manual: bool,
    auto: bool,
    metadata: Value,
}

fn direct_subagent_route(
    prompt: &str,
    subagents: &[TaskSubagentDescriptor],
) -> Option<DirectSubagentRoute> {
    if let Some(tool_call) = manual_subagent_tool_call(prompt) {
        return Some(DirectSubagentRoute {
            tool_call,
            source: "manual_subagent",
            manual: true,
            auto: false,
            metadata: Value::Null,
        });
    }
    let route = select_task_subagent_for_prompt(subagents, prompt)?;
    Some(DirectSubagentRoute {
        tool_call: auto_subagent_tool_call(prompt, &route),
        source: "auto_subagent",
        manual: false,
        auto: true,
        metadata: json!({
            "subagent_type": route.subagent_id.clone(),
            "score": route.score,
            "matched_terms": route.matched_terms.clone(),
        }),
    })
}

fn benchmark_mode_disables_cli_subagents() -> bool {
    std::env::var("OPENAGENT_BENCHMARK_MODE")
        .ok()
        .as_deref()
        .is_some_and(benchmark_mode_value_allows_shell_command)
}

fn auto_subagent_tool_call(prompt: &str, route: &TaskSubagentRoute) -> ToolCall {
    ToolCall {
        name: TASK_TOOL_ID.to_string(),
        input: json!({
            "description": format!("Auto-routed to {}", route.subagent_id),
            "prompt": prompt,
            "subagent_type": route.subagent_id.clone(),
            "command": "auto_route",
        }),
        call_id: format!("auto_task_{}", route.subagent_id),
    }
}

struct TaskExecutionContext<'a> {
    args: &'a [String],
    workspace: &'a Path,
    provider: &'a str,
    model_id: &'a str,
    session: &'a Session,
    store: &'a FileSessionStore,
    run_id: &'a str,
    max_steps: u64,
    permission_ruleset: PermissionRuleset,
    skip_permissions: bool,
}

fn execute_loop_tool_call(
    toolkit: &Toolkit,
    mcp_runtime: Option<&McpRuntime>,
    tool_call: &ToolCall,
    ctx: &mut ToolContext,
    task_context: TaskExecutionContext<'_>,
) -> ToolResult {
    if tool_call.name == "skill" {
        if let Some(result) =
            toolkit.permission_result_for_tool("skill", &tool_call.input, &tool_call.call_id, ctx)
        {
            return result;
        }
        match fork_skill_task_from_input(&tool_call.input, ctx) {
            Ok(Some(fork)) => {
                let task_call = ToolCall {
                    call_id: tool_call.call_id.clone(),
                    name: TASK_TOOL_ID.to_string(),
                    input: fork.task_input,
                };
                let mut result = execute_task_tool_call(toolkit, &task_call, ctx, task_context);
                result
                    .metadata
                    .insert("skill_context".to_string(), json!("fork"));
                result
                    .metadata
                    .insert("skill_name".to_string(), json!(fork.skill_name));
                result
                    .metadata
                    .insert("skill_agent".to_string(), json!(fork.agent));
                result
                    .metadata
                    .insert("background".to_string(), json!(fork.background));
                return result;
            }
            Ok(None) => {}
            Err(error) => {
                return ToolResult {
                    call_id: tool_call.call_id.clone(),
                    output: String::new(),
                    error: Some(error),
                    metadata: BTreeMap::from([
                        ("tool".to_string(), json!("skill")),
                        ("error_kind".to_string(), json!("fork_skill_error")),
                        ("call_id".to_string(), json!(tool_call.call_id.clone())),
                    ]),
                };
            }
        }
    }
    if tool_call.name == TASK_TOOL_ID {
        execute_task_tool_call(toolkit, tool_call, ctx, task_context)
    } else {
        execute_agent_tool(toolkit, mcp_runtime, tool_call, ctx)
    }
}

fn execute_task_tool_call(
    toolkit: &Toolkit,
    tool_call: &ToolCall,
    ctx: &mut ToolContext,
    task_context: TaskExecutionContext<'_>,
) -> ToolResult {
    if let Some(result) =
        toolkit.permission_result_for_tool(TASK_TOOL_ID, &tool_call.input, &tool_call.call_id, ctx)
    {
        return result;
    }
    let input = &tool_call.input;
    if input
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return task_tool_error(
            tool_call,
            "background subagent tasks are not implemented yet; omit background or set it to false",
            BTreeMap::new(),
        );
    }
    let subagent_type = match task_input_string(input, "subagent_type")
        .or_else(|_| task_input_string(input, "agent_type"))
        .or_else(|_| task_input_string(input, "agent"))
    {
        Ok(value) => value,
        Err(error) => return task_tool_error(tool_call, &error, BTreeMap::new()),
    };
    let prompt = match task_input_string(input, "prompt") {
        Ok(value) => value,
        Err(error) => return task_tool_error(tool_call, &error, BTreeMap::new()),
    };
    let description =
        task_input_string(input, "description").unwrap_or_else(|_| subagent_type.clone());
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let child_profile = match load_agent_profile_by_name(task_context.args, &subagent_type) {
        Ok(profile) => profile,
        Err(error) => {
            return task_tool_error(
                tool_call,
                &error,
                BTreeMap::from([("subagent_type".to_string(), json!(subagent_type))]),
            );
        }
    };
    if !is_subagent_mode(&child_profile.mode) {
        return task_tool_error(
            tool_call,
            &format!(
                "agent profile {} has mode {}; task can only launch subagent or all profiles",
                child_profile.id, child_profile.mode
            ),
            BTreeMap::from([("subagent_type".to_string(), json!(subagent_type))]),
        );
    }
    let child_permission = match permission_ruleset_for_profile(
        &child_profile,
        task_context.permission_ruleset.clone(),
    ) {
        Ok(value) => value,
        Err(error) => return task_tool_error(tool_call, &error, BTreeMap::new()),
    };
    let (child_provider, child_model) = provider_and_model_for_subagent(
        task_context.provider,
        task_context.model_id,
        &child_profile,
    );
    let mut child_session = match task_id.as_deref() {
        Some(existing) => match task_context.store.load_session(existing) {
            Ok(session) => session,
            Err(error) => {
                return task_tool_error(
                    tool_call,
                    &format!("failed to resume task session {existing}: {error}"),
                    BTreeMap::from([
                        ("subagent_type".to_string(), json!(subagent_type)),
                        ("task_id".to_string(), json!(existing)),
                    ]),
                );
            }
        },
        None => Session::new(new_cli_id("subtask"), task_context.workspace),
    };
    let mut workspace_isolation = None;
    if let Some(existing) = task_id.as_deref() {
        if let Err(error) = validate_task_resume_session(
            &child_session,
            task_context.session,
            &child_profile,
            &subagent_type,
            existing,
        ) {
            return task_tool_error(
                tool_call,
                &error,
                BTreeMap::from([
                    ("subagent_type".to_string(), json!(subagent_type)),
                    ("task_id".to_string(), json!(existing)),
                ]),
            );
        }
    }
    if task_id.is_none()
        && task_workspace_isolation_requested(input, child_profile.workspace_isolation)
    {
        match prepare_isolated_workspace(
            task_context.workspace,
            task_context.store.root.join("isolated_workspaces"),
            &child_session.id,
        ) {
            Ok(isolation) => {
                child_session.directory = PathBuf::from(&isolation.workspace);
                workspace_isolation = Some(isolation);
            }
            Err(error) => {
                return task_tool_error(
                    tool_call,
                    &format!("failed to prepare isolated workspace: {error}"),
                    BTreeMap::from([("subagent_type".to_string(), json!(subagent_type))]),
                );
            }
        }
    }
    if let Some(error) = subagent_task_governance_error(task_context.session, &child_profile) {
        return task_tool_error(
            tool_call,
            &error,
            BTreeMap::from([
                ("tool".to_string(), json!(TASK_TOOL_ID)),
                ("subagent_type".to_string(), json!(subagent_type)),
                ("status".to_string(), json!("failed")),
                (
                    "task_depth".to_string(),
                    json!(child_task_depth(task_context.session)),
                ),
                (
                    "max_task_depth".to_string(),
                    json!(max_subagent_depth_cli()),
                ),
                (
                    "task_lineage_subagents".to_string(),
                    json!(parent_task_lineage(task_context.session)),
                ),
            ]),
        );
    }
    let task_depth = child_task_depth(task_context.session);
    let task_root_id = task_root_session_id(task_context.session);
    let task_lineage_subagents = child_task_lineage(task_context.session, &child_profile.id);
    let child_run_id = new_cli_id("run");
    let trace_id = new_cli_id("trace");
    child_session.status = SessionStatus::Running;
    child_session
        .metadata
        .insert("agent".to_string(), json!(child_profile.id.clone()));
    child_session
        .metadata
        .insert("provider".to_string(), json!(child_provider.clone()));
    child_session
        .metadata
        .insert("model".to_string(), json!(child_model.clone()));
    child_session.metadata.insert(
        "model_options".to_string(),
        json!(child_profile.model_options.clone()),
    );
    if let Some(temperature) = child_profile.temperature {
        child_session
            .metadata
            .insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = child_profile.top_p {
        child_session
            .metadata
            .insert("top_p".to_string(), json!(top_p));
    }
    if let Some(color) = child_profile.color.as_deref() {
        child_session
            .metadata
            .insert("color".to_string(), json!(color));
    }
    child_session
        .metadata
        .insert("subagent".to_string(), json!(true));
    child_session.metadata.insert(
        "agent_profile".to_string(),
        agent_profile_public_value(&child_profile),
    );
    child_session.metadata.insert(
        "parent_session_id".to_string(),
        json!(task_context.session.id.clone()),
    );
    child_session.metadata.insert(
        "task_parent_session_id".to_string(),
        json!(task_context.session.id.clone()),
    );
    child_session.metadata.insert(
        "task_root_session_id".to_string(),
        json!(task_root_id.clone()),
    );
    child_session
        .metadata
        .insert("task_depth".to_string(), json!(task_depth));
    child_session.metadata.insert(
        "task_lineage_subagents".to_string(),
        json!(task_lineage_subagents.clone()),
    );
    child_session
        .metadata
        .insert("parent_run_id".to_string(), json!(task_context.run_id));
    child_session.metadata.insert(
        "parent_tool_call_id".to_string(),
        json!(tool_call.call_id.clone()),
    );
    child_session
        .metadata
        .insert("task_description".to_string(), json!(description.clone()));
    child_session.metadata.insert(
        "task_subagent_type".to_string(),
        json!(subagent_type.clone()),
    );
    if let Some(command) = command.as_deref() {
        child_session
            .metadata
            .insert("task_command".to_string(), json!(command));
    }
    if let Some(isolation) = workspace_isolation.as_ref() {
        child_session
            .metadata
            .insert("workspace_isolation".to_string(), json!(isolation));
    }
    child_session
        .metadata
        .insert("permission".to_string(), json!(child_permission.as_str()));
    if task_id.is_some() {
        let resume_count = child_session
            .metadata
            .get("task_resume_count")
            .and_then(Value::as_u64)
            .unwrap_or_default()
            .saturating_add(1);
        child_session
            .metadata
            .insert("task_resume_count".to_string(), json!(resume_count));
        child_session
            .metadata
            .insert("task_resumed_at_ms".to_string(), json!(now_ms_cli()));
    }
    let child_max_steps = child_profile.max_steps.unwrap_or(task_context.max_steps);
    if let Err(error) = task_context.store.start_run(
        &mut child_session,
        StartRunOptions {
            run_id: child_run_id.clone(),
            trace_id,
            agent_name: child_profile.id.clone(),
            model_id: Some(child_model.clone()),
            provider_id: Some(child_provider.clone()),
            permission: if task_context.skip_permissions {
                format!("auto_allow:{}", child_permission.as_str())
            } else {
                child_permission.as_str().to_string()
            },
            max_steps: child_max_steps,
            started_at_ms: None,
        },
    ) {
        return task_tool_error(
            tool_call,
            &format!("failed to start subagent session: {error}"),
            BTreeMap::from([("subagent_type".to_string(), json!(subagent_type))]),
        );
    }
    let user_message = chat_message(Role::User, prompt.clone());
    let user_index = child_session.messages.len() as u64;
    child_session.add(user_message.clone());
    if let Err(error) =
        task_context
            .store
            .append_message(&child_session, &user_message, &child_run_id, user_index)
    {
        return task_tool_error(
            tool_call,
            &format!("failed to record subagent prompt: {error}"),
            BTreeMap::new(),
        );
    }

    let mut child_event_sink: Option<&mut dyn FnMut(&Value)> = None;
    let child_workspace = child_session.directory.clone();
    let child_loop_result = run_agent_loop(
        AgentLoopRequest {
            args: task_context.args,
            workspace: &child_workspace,
            provider: &child_provider,
            model_id: &child_model,
            session: &mut child_session,
            store: task_context.store,
            run_id: &child_run_id,
            max_steps: child_max_steps,
            prompt: &prompt,
            agent_profile: Some(&child_profile),
            permission_ruleset: child_permission.clone(),
            skip_permissions: task_context.skip_permissions,
        },
        &mut child_event_sink,
    );

    match child_loop_result {
        Ok(result) => {
            child_session.status = SessionStatus::Idle;
            let _ = task_context.store.record_event(
                &child_session.id,
                &child_run_id,
                "model.usage",
                SessionEventOptions {
                    kind: "model".to_string(),
                    attributes: BTreeMap::from([
                        ("input_tokens".to_string(), json!(result.usage.input_tokens)),
                        (
                            "output_tokens".to_string(),
                            json!(result.usage.output_tokens),
                        ),
                        ("cost".to_string(), json!(result.usage.cost)),
                        ("source".to_string(), json!(result.source.clone())),
                        ("tool_calls".to_string(), json!(result.tool_calls)),
                    ]),
                    ..SessionEventOptions::default()
                },
            );
            let _ = task_context.store.finish_run(
                &child_session,
                &child_run_id,
                "completed",
                result.steps.max(1),
                Some(&result.finish_reason),
                None,
            );
            let output = render_task_output(&child_session.id, "completed", &result.answer);
            let mut metadata = BTreeMap::from([
                ("tool".to_string(), json!(TASK_TOOL_ID)),
                ("title".to_string(), json!(description)),
                ("subagent_type".to_string(), json!(subagent_type)),
                ("task_id".to_string(), json!(child_session.id.clone())),
                ("session_id".to_string(), json!(child_session.id.clone())),
                ("run_id".to_string(), json!(child_run_id)),
                ("status".to_string(), json!("completed")),
                ("provider".to_string(), json!(child_provider)),
                ("model".to_string(), json!(child_model)),
                (
                    "model_options".to_string(),
                    json!(child_profile.model_options.clone()),
                ),
                ("task_depth".to_string(), json!(task_depth)),
                (
                    "task_root_session_id".to_string(),
                    json!(task_root_id.clone()),
                ),
                (
                    "task_parent_session_id".to_string(),
                    json!(task_context.session.id.clone()),
                ),
                (
                    "task_lineage_subagents".to_string(),
                    json!(task_lineage_subagents.clone()),
                ),
                ("steps".to_string(), json!(result.steps)),
                ("tool_calls".to_string(), json!(result.tool_calls)),
                (
                    "agent_profile".to_string(),
                    agent_profile_public_value(&child_profile),
                ),
            ]);
            if let Some(isolation) = workspace_isolation.as_ref() {
                metadata.insert("workspace_isolation".to_string(), json!(isolation));
            }
            ToolResult {
                call_id: tool_call.call_id.clone(),
                output,
                error: None,
                metadata,
            }
        }
        Err(error) => {
            child_session.status = if error.paused {
                SessionStatus::Paused
            } else {
                SessionStatus::Stop
            };
            let finish_reason = error.finish_reason.as_deref().unwrap_or(if error.paused {
                "paused"
            } else {
                "error"
            });
            let _ = task_context.store.finish_run(
                &child_session,
                &child_run_id,
                "failed",
                error.steps.max(1),
                Some(finish_reason),
                Some(&error.message),
            );
            task_tool_error(
                tool_call,
                &format!("subagent {subagent_type} failed: {}", error.message),
                BTreeMap::from([
                    ("tool".to_string(), json!(TASK_TOOL_ID)),
                    ("title".to_string(), json!(description)),
                    ("subagent_type".to_string(), json!(subagent_type)),
                    ("task_id".to_string(), json!(child_session.id.clone())),
                    ("session_id".to_string(), json!(child_session.id.clone())),
                    ("run_id".to_string(), json!(child_run_id)),
                    (
                        "status".to_string(),
                        json!(if error.paused { "paused" } else { "failed" }),
                    ),
                    ("provider".to_string(), json!(child_provider)),
                    ("model".to_string(), json!(child_model)),
                    (
                        "model_options".to_string(),
                        json!(child_profile.model_options.clone()),
                    ),
                    ("task_depth".to_string(), json!(task_depth)),
                    ("task_root_session_id".to_string(), json!(task_root_id)),
                    (
                        "task_parent_session_id".to_string(),
                        json!(task_context.session.id.clone()),
                    ),
                    (
                        "task_lineage_subagents".to_string(),
                        json!(task_lineage_subagents),
                    ),
                    ("paused".to_string(), json!(error.paused)),
                ]),
            )
        }
    }
}

fn validate_task_resume_session(
    child_session: &Session,
    parent_session: &Session,
    profile: &RunAgentProfile,
    requested_subagent_type: &str,
    task_id: &str,
) -> Result<(), String> {
    if !child_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!("task session {task_id} is not a subagent task"));
    }
    let parent_id = child_session
        .metadata
        .get("parent_session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if parent_id != parent_session.id {
        return Err("task does not belong to parent session".to_string());
    }
    let stored_agent = child_session
        .metadata
        .get("agent")
        .and_then(Value::as_str)
        .or_else(|| {
            child_session
                .metadata
                .get("task_subagent_type")
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    if !stored_agent.is_empty()
        && stored_agent != profile.id
        && stored_agent != requested_subagent_type
    {
        return Err(format!(
            "task session {task_id} belongs to subagent {stored_agent}, not {}",
            profile.id
        ));
    }
    match child_session
        .metadata
        .get("task_status")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "queued" | "running" | "canceled" => {
            return Err(format!(
                "task session {task_id} cannot be resumed while task status is {}",
                child_session
                    .metadata
                    .get("task_status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ));
        }
        _ => {}
    }
    if matches!(
        child_session.status,
        SessionStatus::Running | SessionStatus::Paused | SessionStatus::Compacting
    ) {
        return Err(format!(
            "task session {task_id} cannot be resumed while session status is {}",
            task_session_status_text(&child_session.status)
        ));
    }
    Ok(())
}

fn task_session_status_text(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Running => "running",
        SessionStatus::Paused => "paused",
        SessionStatus::Stop => "stop",
        SessionStatus::Compacting => "compacting",
    }
}

fn task_input_string(input: &Value, key: &str) -> Result<String, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("task tool requires non-empty {key}"))
}

fn task_workspace_isolation_requested(input: &Value, profile_default: bool) -> bool {
    input
        .get("isolate_workspace")
        .or_else(|| input.get("workspace_isolation"))
        .and_then(Value::as_bool)
        .unwrap_or(profile_default)
}

fn task_tool_error(
    tool_call: &ToolCall,
    error: &str,
    mut metadata: BTreeMap<String, Value>,
) -> ToolResult {
    metadata
        .entry("tool".to_string())
        .or_insert_with(|| json!(TASK_TOOL_ID));
    ToolResult {
        call_id: tool_call.call_id.clone(),
        output: String::new(),
        error: Some(error.to_string()),
        metadata,
    }
}

fn render_task_output(task_id: &str, state: &str, text: &str) -> String {
    format!(
        "<task id=\"{}\" state=\"{}\">\n<task_result>\n{}\n</task_result>\n</task>",
        escape_task_text(task_id),
        escape_task_text(state),
        escape_task_text(text),
    )
}

fn escape_task_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(super) fn pending_resume_from_session(session: &Session) -> Option<PendingResume> {
    if let Some(response) = session.metadata.get("pending_question_response")
        && let Some(pending) = session.metadata.get("pending_question")
    {
        return pending_resume_from_values("question", pending, response);
    }
    if let Some(response) = session.metadata.get("pending_approval_response")
        && let Some(pending) = session.metadata.get("pending_approval")
    {
        return pending_resume_from_values("approval", pending, response);
    }
    None
}

fn pending_resume_from_values(
    kind: &str,
    pending: &Value,
    response: &Value,
) -> Option<PendingResume> {
    let call_id = pending.get("call_id").and_then(Value::as_str)?.to_string();
    let tool_name = pending
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or(if kind == "question" { "question" } else { "" })
        .to_string();
    if tool_name.is_empty() {
        return None;
    }
    Some(PendingResume {
        kind: kind.to_string(),
        request_id: pending
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or(&call_id)
            .to_string(),
        call: ToolCall {
            name: tool_name,
            input: pending
                .get("tool_input")
                .or_else(|| pending.get("toolInput"))
                .cloned()
                .unwrap_or_else(|| json!({})),
            call_id,
        },
        response: response.clone(),
        step: pending.get("step").and_then(Value::as_u64).unwrap_or(0),
    })
}

pub(super) struct PendingResumeContext<'a, 'sink> {
    pub(super) args: &'a [String],
    pub(super) workspace: &'a Path,
    pub(super) provider: &'a str,
    pub(super) model_id: &'a str,
    pub(super) toolkit: &'a Toolkit,
    pub(super) mcp_runtime: Option<&'a McpRuntime>,
    pub(super) ctx: &'a mut ToolContext,
    pub(super) session: &'a mut Session,
    pub(super) store: &'a FileSessionStore,
    pub(super) run_id: &'a str,
    pub(super) max_steps: u64,
    pub(super) permission_ruleset: PermissionRuleset,
    pub(super) skip_permissions: bool,
    pub(super) events: &'a mut Vec<Value>,
    pub(super) event_sink: &'a mut Option<&'sink mut dyn FnMut(&Value)>,
}

fn process_pending_resume(
    pending: PendingResume,
    context: &mut PendingResumeContext<'_, '_>,
) -> Result<(), String> {
    emit_run_event(
        context.events,
        json!({
            "method": format!("turn/{}_resumed", pending.kind),
            "params": {
                "session_id": context.session.id.clone(),
                "run_id": context.run_id,
                "request_id": pending.request_id.clone(),
                "call_id": pending.call.call_id.clone(),
            }
        }),
        context.event_sink,
    );
    let result = if pending.kind == "question" {
        let answers = pending
            .response
            .get("answers")
            .and_then(question_answers_from_json)
            .or_else(|| {
                pending
                    .response
                    .get("answer")
                    .and_then(value_to_answer_string)
                    .map(|answer| vec![vec![answer]])
            })
            .unwrap_or_default();
        context.ctx.set_question_answers(answers);
        context.toolkit.execute(
            "question",
            pending.call.input.clone(),
            &pending.call.call_id,
            context.ctx,
        )
    } else {
        let decision = pending
            .response
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("allow_once");
        if matches!(decision, "reject" | "deny") {
            ToolResult {
                call_id: pending.call.call_id.clone(),
                output: String::new(),
                error: Some(
                    pending
                        .response
                        .get("note")
                        .and_then(Value::as_str)
                        .unwrap_or("Permission rejected by user")
                        .to_string(),
                ),
                metadata: BTreeMap::from([
                    ("tool".to_string(), json!(pending.call.name.clone())),
                    ("permission_action".to_string(), json!("reject")),
                    ("request_id".to_string(), json!(pending.request_id.clone())),
                ]),
            }
        } else {
            if matches!(decision, "allow_always" | "always")
                && let Some(pattern) = context
                    .session
                    .metadata
                    .get("pending_approval")
                    .and_then(|item| item.get("permission_pattern"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            {
                add_approval_always_pattern(context.session, pattern);
            }
            let previous = context.ctx.dangerously_skip_permissions;
            context.ctx.dangerously_skip_permissions = true;
            let result = execute_loop_tool_call(
                context.toolkit,
                context.mcp_runtime,
                &pending.call,
                context.ctx,
                TaskExecutionContext {
                    args: context.args,
                    workspace: context.workspace,
                    provider: context.provider,
                    model_id: context.model_id,
                    session: context.session,
                    store: context.store,
                    run_id: context.run_id,
                    max_steps: context.max_steps,
                    permission_ruleset: context.permission_ruleset.clone(),
                    skip_permissions: context.skip_permissions,
                },
            );
            context.ctx.dangerously_skip_permissions = previous;
            result
        }
    };
    append_tool_result_to_session(context, pending.step, &pending.call, result)?;
    context.session.metadata.remove("pending_question");
    context.session.metadata.remove("pending_question_response");
    context.session.metadata.remove("pending_approval");
    context.session.metadata.remove("pending_approval_response");
    context
        .store
        .save_state(context.session, Some(context.run_id))
        .map_err(|error| format!("failed to save resumed session state: {error}"))?;
    Ok(())
}

fn append_tool_result_to_session(
    context: &mut PendingResumeContext<'_, '_>,
    step: u64,
    tool_call: &ToolCall,
    tool_result: ToolResult,
) -> Result<(), String> {
    let runner_facade = SessionRunnerFacade::new(context.workspace, context.session.id.clone());
    emit_run_event(
        context.events,
        runner_facade.tool_call_finished_event(
            context.run_id,
            step,
            tool_call,
            &tool_result,
            None,
            BTreeMap::new(),
        ),
        context.event_sink,
    );
    let assistant_message_id = context
        .session
        .metadata
        .get(if tool_call.name == "question" {
            "pending_question"
        } else {
            "pending_approval"
        })
        .and_then(|value| value.get("assistant_message_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let settlement = runner_facade.tool_result_settlement(
        step,
        tool_call,
        &tool_result,
        assistant_message_id.as_deref(),
        Some(new_cli_id("msg")),
    );
    for intent in &settlement.event_intents {
        let _ = context.store.record_event(
            &context.session.id,
            context.run_id,
            &intent.event_name,
            SessionEventOptions {
                kind: intent.kind.clone(),
                status: intent.status.clone(),
                attributes: intent.attributes.clone(),
                ..SessionEventOptions::default()
            },
        );
    }
    let part_intent = &settlement.part_intent;
    let _ = context.store.append_part(
        &context.session.id,
        context.run_id,
        &part_intent.part_type,
        SessionPartOptions {
            attributes: part_intent.attributes.clone(),
            step_index: part_intent.step_index,
            status: part_intent.status.clone(),
            ..SessionPartOptions::default()
        },
    );
    let tool_message = settlement.message;
    let tool_index = context.session.messages.len() as u64;
    context.session.add(tool_message.clone());
    context
        .store
        .append_message(context.session, &tool_message, context.run_id, tool_index)
        .map_err(|error| format!("failed to record resumed tool message: {error}"))
}

fn emit_run_event(
    events: &mut Vec<Value>,
    event: Value,
    event_sink: &mut Option<&mut dyn FnMut(&Value)>,
) {
    if let Some(emit) = event_sink.as_deref_mut() {
        emit(&event);
    }
    events.push(event);
}

fn create_step_checkpoint(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    workspace: &Path,
    step: u64,
    kind: &str,
    message_id: &str,
) -> Option<String> {
    store
        .create_checkpoint(
            session_id,
            run_id,
            workspace,
            kind,
            Some(message_id),
            None,
            Some(step),
        )
        .ok()
        .map(|checkpoint| checkpoint.checkpoint_id)
}

fn finalize_step_checkpoint(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    workspace: &Path,
    step: u64,
    message_id: &str,
    start_checkpoint_id: Option<&str>,
) {
    let Some(end_checkpoint_id) = create_step_checkpoint(
        store, session_id, run_id, workspace, step, "step_end", message_id,
    ) else {
        return;
    };
    let _ = store.append_part(
        session_id,
        run_id,
        "context",
        SessionPartOptions {
            message_id: Some(message_id.to_string()),
            content: Some(json!({
                "kind": "checkpoint",
                "snapshot_start": start_checkpoint_id,
                "snapshot_end": end_checkpoint_id,
            })),
            attributes: BTreeMap::from([
                ("kind".to_string(), json!("checkpoint")),
                ("snapshot_start".to_string(), json!(start_checkpoint_id)),
                ("snapshot_end".to_string(), json!(end_checkpoint_id.clone())),
            ]),
            step_index: Some(step),
            status: "completed".to_string(),
            ..SessionPartOptions::default()
        },
    );
    if let Some(start_checkpoint_id) = start_checkpoint_id {
        let _ = store.append_checkpoint_patch_part(
            session_id,
            run_id,
            message_id,
            start_checkpoint_id,
            &end_checkpoint_id,
            Some(step),
        );
    }
}

fn record_step_started(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    step: u64,
    checkpoint_id: Option<&str>,
) {
    let _ = store.record_event(
        session_id,
        run_id,
        "step.started",
        SessionEventOptions {
            kind: "step".to_string(),
            attributes: BTreeMap::from([
                ("step".to_string(), json!(step)),
                ("checkpoint_id".to_string(), json!(checkpoint_id)),
            ]),
            ..SessionEventOptions::default()
        },
    );
}

fn record_step_finished(
    store: &FileSessionStore,
    session_id: &str,
    run_id: &str,
    step: u64,
    finish_reason: &str,
    tool_calls: u64,
    usage: &Usage,
) {
    let _ = store.record_event(
        session_id,
        run_id,
        "step.finished",
        SessionEventOptions {
            kind: "step".to_string(),
            attributes: BTreeMap::from([
                ("step".to_string(), json!(step)),
                ("finish_reason".to_string(), json!(finish_reason)),
                ("tool_calls".to_string(), json!(tool_calls)),
                ("input_tokens".to_string(), json!(usage.input_tokens)),
                ("output_tokens".to_string(), json!(usage.output_tokens)),
            ]),
            ..SessionEventOptions::default()
        },
    );
}

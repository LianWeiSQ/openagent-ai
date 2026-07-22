use super::*;

const MAX_CONTEXT_PACK_RECEIPTS: usize = 64;

pub(super) struct CliContextPackRequest<'a> {
    pub(super) args: &'a [String],
    pub(super) provider: &'a str,
    pub(super) model_id: &'a str,
    pub(super) session: &'a mut Session,
    pub(super) store: &'a FileSessionStore,
    pub(super) run_id: &'a str,
    pub(super) step: u64,
    pub(super) tools: &'a [ToolSchema],
    pub(super) mcp_runtime: Option<&'a McpRuntime>,
    pub(super) agent_profile: Option<&'a RunAgentProfile>,
}

pub(super) fn build_cli_context_pack(
    request: CliContextPackRequest<'_>,
) -> Result<ContextPack, String> {
    let CliContextPackRequest {
        args,
        provider,
        model_id,
        session,
        store,
        run_id,
        step,
        tools,
        mcp_runtime,
        agent_profile,
    } = request;
    let history = materialize_context_history(
        store
            .materialized_chat_messages(session)
            .unwrap_or_else(|_| session.messages.clone()),
    );
    let messages = history.messages;
    let mut system_sources = profile::context_system_sources(
        session,
        agent_profile,
        agent_profile.map_or("", |profile| profile.mode.as_str()),
    );
    system_sources.legacy_system_sources = history.legacy_system_sources;
    let work_state = history.work_state.or_else(|| {
        session
            .metadata
            .get("compact")
            .and_then(|compact| context_work_state_from_compact_metadata(compact, messages.len()))
    });
    let attachments = context_attachments_from_messages(&messages);
    let todos = context_todos(session);
    let checkpoints = context_checkpoints(store, session);
    let tool_manifests = mcp_tool_manifest_items(mcp_runtime, tools);
    let model_options = model_options_from_cli(args, agent_profile);
    let build_options = context_build_options(args, provider, model_id, agent_profile)?;
    let pack = ContextPackBuilder::new(Some(build_options)).build(ContextPackInput {
        system_sources: Some(system_sources),
        messages,
        tools: tools.to_vec(),
        model_options,
        attachments,
        work_state,
        todos,
        checkpoints,
        skills: Vec::new(),
        tool_manifests,
        metadata: BTreeMap::new(),
        runtime_context: None,
        sandbox_metadata: None,
        extra_items: Vec::new(),
    });
    profile::apply_context_system_diagnostics(session, pack.system_diagnostics.as_ref());
    persist_context_pack_receipt(store, session, run_id, step, &pack)?;
    Ok(pack)
}

pub(super) fn context_attachments_from_files(files: &[(String, String)]) -> Vec<ContextAttachment> {
    files
        .iter()
        .map(|(path, content)| {
            ContextAttachment::new(
                ContextAttachmentKind::File,
                Some(path.clone()),
                Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string()),
                "text/plain",
                content.len() as u64,
                content.clone(),
            )
        })
        .collect()
}

fn context_attachments_from_messages(messages: &[ChatMessage]) -> Vec<ContextAttachment> {
    messages
        .iter()
        .enumerate()
        .flat_map(|(message_index, message)| {
            message
                .metadata
                .get("context_attachments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |value| {
                    let mut attachment =
                        serde_json::from_value::<ContextAttachment>(value.clone()).ok()?;
                    attachment.id = attachment.stable_id();
                    attachment.source_message_index = Some(message_index);
                    Some(attachment)
                })
        })
        .collect()
}

fn context_todos(session: &Session) -> Vec<ContextTodo> {
    session
        .todos
        .iter()
        .map(|todo| {
            ContextTodo::new(
                Some(todo.id.clone()),
                todo.content.clone(),
                todo.status.clone(),
                todo.priority.clone(),
            )
        })
        .collect()
}

fn context_checkpoints(store: &FileSessionStore, session: &Session) -> Vec<ContextCheckpoint> {
    let restored_id = session
        .metadata
        .get("latest_checkpoint_restore")
        .and_then(Value::as_object)
        .and_then(|restore| restore.get("checkpoint_id"))
        .and_then(Value::as_str);
    let checkpoints = store.list_checkpoints(&session.id).unwrap_or_default();
    let mut selected = checkpoints.iter().take(1).collect::<Vec<_>>();
    if let Some(restored_id) = restored_id
        && let Some(restored) = checkpoints
            .iter()
            .find(|checkpoint| checkpoint.checkpoint_id == restored_id)
        && !selected
            .iter()
            .any(|checkpoint| checkpoint.checkpoint_id == restored_id)
    {
        selected.push(restored);
    }
    selected
        .into_iter()
        .map(|checkpoint| ContextCheckpoint {
            id: checkpoint.checkpoint_id.clone(),
            kind: checkpoint.kind.clone(),
            run_id: checkpoint.run_id.clone(),
            timestamp_ms: checkpoint.timestamp_ms,
            message_id: checkpoint.message_id.clone(),
            part_id: checkpoint.part_id.clone(),
            step_index: checkpoint.step_index,
            file_count: checkpoint.file_count,
            total_bytes: checkpoint.total_bytes,
            restored: restored_id == Some(checkpoint.checkpoint_id.as_str()),
            metadata: BTreeMap::new(),
        })
        .collect()
}

fn mcp_tool_manifest_items(
    mcp_runtime: Option<&McpRuntime>,
    tools: &[ToolSchema],
) -> Vec<ContextItem> {
    let Some(runtime) = mcp_runtime else {
        return Vec::new();
    };
    let visible_tools = tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<BTreeMap<_, _>>();
    runtime
        .descriptors
        .values()
        .filter_map(|descriptor| {
            let tool = visible_tools.get(descriptor.dynamic_name.as_str())?;
            Some(tool_manifest_context_item(
                format!("mcp_tool:{}", descriptor.dynamic_name),
                format!("mcp.server:{}", descriptor.server_name),
                tool,
                BTreeMap::from([
                    ("server_name".to_string(), json!(descriptor.server_name)),
                    ("original_name".to_string(), json!(descriptor.original_name)),
                    ("dynamic_name".to_string(), json!(descriptor.dynamic_name)),
                    ("title".to_string(), json!(descriptor.title)),
                ]),
            ))
        })
        .collect()
}

fn model_options_from_cli(
    args: &[String],
    agent_profile: Option<&RunAgentProfile>,
) -> BTreeMap<String, Value> {
    let mut options = agent_profile
        .map(|profile| profile.model_options.clone())
        .unwrap_or_default();
    options.remove("context_budget");
    if let Some(temperature) = agent_profile.and_then(|profile| profile.temperature) {
        options.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = agent_profile.and_then(|profile| profile.top_p) {
        options.insert("top_p".to_string(), json!(top_p));
    }
    if let Some(reasoning_effort) = value_for(args, &["--variant"]) {
        options.insert("reasoning_effort".to_string(), json!(reasoning_effort));
    } else if has_flag(args, &["--thinking"]) {
        options.insert("reasoning_effort".to_string(), json!("high"));
    }
    if let Some(max_output_tokens) =
        value_for(args, &["--max-output-tokens"]).and_then(|value| value.parse::<u64>().ok())
    {
        options.insert("max_output_tokens".to_string(), json!(max_output_tokens));
    }
    options
}

fn context_build_options(
    args: &[String],
    provider: &str,
    model_id: &str,
    agent_profile: Option<&RunAgentProfile>,
) -> Result<openagent_core::ContextPackBuildOptions, String> {
    let context_window = value_for(args, &["--context-window"])
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            env::var("OPENAGENT_CONTEXT_WINDOW")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        });
    let max_output = value_for(args, &["--max-output-tokens"])
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            env::var("OPENAGENT_MAX_OUTPUT_TOKENS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        });
    let model = openagent_context_model(provider, model_id, context_window, max_output);
    let context_budget = agent_profile
        .and_then(|profile| profile.model_options.get("context_budget"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    context_pack_build_options_for_model(
        Some(&json!({"context_budget": context_budget})),
        &model,
        false,
    )
}

fn persist_context_pack_receipt(
    store: &FileSessionStore,
    session: &mut Session,
    run_id: &str,
    step: u64,
    pack: &ContextPack,
) -> Result<(), String> {
    let mut receipts = session
        .metadata
        .get("context_pack_receipts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prefix_cache = context_prefix_cache_status(&receipts, pack, run_id, step);
    let envelope = json!({
        "schema_version": "openagent.turn_context_pack.v1",
        "mode": "active",
        "surface": "cli",
        "run_id": run_id,
        "step": step,
        "receipt": pack.receipt,
        "system_diagnostics": pack.system_diagnostics,
        "prefix_cache": prefix_cache,
    });
    let already_persisted = receipts.iter().any(|existing| {
        existing.get("run_id").and_then(Value::as_str) == Some(run_id)
            && existing.get("step").and_then(Value::as_u64) == Some(step)
            && existing
                .pointer("/receipt/pack_hash")
                .and_then(Value::as_str)
                == Some(pack.pack_hash.as_str())
    });
    receipts.retain(|existing| {
        existing.get("run_id").and_then(Value::as_str) != Some(run_id)
            || existing.get("step").and_then(Value::as_u64) != Some(step)
    });
    receipts.push(envelope.clone());
    if receipts.len() > MAX_CONTEXT_PACK_RECEIPTS {
        receipts.drain(..receipts.len() - MAX_CONTEXT_PACK_RECEIPTS);
    }
    session
        .metadata
        .insert("context_pack".to_string(), envelope);
    session
        .metadata
        .insert("context_pack_receipts".to_string(), Value::Array(receipts));
    if !already_persisted {
        store
            .record_event(
                &session.id,
                run_id,
                "context.pack_built",
                SessionEventOptions {
                    kind: "context".to_string(),
                    attributes: BTreeMap::from([
                        ("mode".to_string(), json!("active")),
                        ("surface".to_string(), json!("cli")),
                        ("step".to_string(), json!(step)),
                        ("receipt".to_string(), json!(pack.receipt)),
                        ("prefix_cache".to_string(), prefix_cache),
                    ]),
                    ..SessionEventOptions::default()
                },
            )
            .map_err(|error| error.to_string())?;
    }
    store
        .save_state(session, Some(run_id))
        .map_err(|error| error.to_string())
}

fn context_prefix_cache_status(
    receipts: &[Value],
    pack: &ContextPack,
    run_id: &str,
    step: u64,
) -> Value {
    let prefix = &pack.stable_prefix;
    if !prefix.cache_eligible {
        return json!({
            "schema_version": "openagent.context_prefix_cache.v1",
            "scope": "logical_prefix_reuse",
            "status": "bypass",
            "cache_eligible": false,
            "stable_prefix_hash": prefix.hash,
            "stable_prefix_token_estimate": prefix.token_estimate,
            "retry_reuses_pack": true,
        });
    }
    let previous = receipts.iter().rev().find(|receipt| {
        (receipt.get("run_id").and_then(Value::as_str) != Some(run_id)
            || receipt.get("step").and_then(Value::as_u64) != Some(step))
            && receipt
                .pointer("/receipt/stable_prefix/cache_eligible")
                .and_then(Value::as_bool)
                == Some(true)
    });
    let reused = receipts.iter().rev().find(|receipt| {
        (receipt.get("run_id").and_then(Value::as_str) != Some(run_id)
            || receipt.get("step").and_then(Value::as_u64) != Some(step))
            && receipt
                .pointer("/receipt/stable_prefix/hash")
                .and_then(Value::as_str)
                == Some(prefix.hash.as_str())
    });
    let status = if reused.is_some() {
        "reused"
    } else if previous.is_some() {
        "changed"
    } else {
        "miss"
    };
    json!({
        "schema_version": "openagent.context_prefix_cache.v1",
        "scope": "logical_prefix_reuse",
        "status": status,
        "cache_eligible": true,
        "stable_prefix_hash": prefix.hash,
        "stable_prefix_token_estimate": prefix.token_estimate,
        "retry_reuses_pack": true,
        "reused_from": reused.map(|receipt| json!({
            "run_id": receipt.get("run_id").cloned().unwrap_or(Value::Null),
            "step": receipt.get("step").cloned().unwrap_or(Value::Null),
        })),
    })
}

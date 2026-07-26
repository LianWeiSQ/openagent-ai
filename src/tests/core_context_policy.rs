use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use openagent_core::{
    CONTEXT_FAILURE_SCHEMA_VERSION, CONTEXT_PACK_RECEIPT_SCHEMA_VERSION,
    CONTEXT_PACK_SCHEMA_VERSION, CONTEXT_PERFORMANCE_SCHEMA_VERSION, ContextAttachment,
    ContextAttachmentKind, ContextCheckpoint, ContextDelivery, ContextFailure, ContextFailureCode,
    ContextItem, ContextPackBuildOptions, ContextPackBuilder, ContextPackInput,
    ContextPackPerformance, ContextSystemSources, ContextTodo, ContextWorkState,
    InstructionContextLoader, InstructionLoadOptions, PermissionManager, SkillDocument, SkillInfo,
    SkillRegistry, SkillRegistryOptions, check_context_budget, context_provider_input_hash,
    estimate_text_tokens, format_context_budget_error, load_context_budget_options,
    materialize_context_history, pattern_for, permission_rule, skill_context_items,
    tool_manifest_context_item,
};
use openagent_protocol::{
    ChatMessage, Model, ModelCapabilities, ModelPricing, PermissionAction, PermissionRuleset, Role,
    ToolSchema,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn core_context_policy_fixture_matches_legacy_oracle() -> Result<(), Box<dyn Error>> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/golden/rust_rewrite/core_context_policy.json"
    ))?;
    assert_eq!(fixture, core_context_policy_fixture()?);
    Ok(())
}

#[test]
fn context_pack_contract_is_deterministic_and_receipt_is_redacted() -> Result<(), Box<dyn Error>> {
    let secret = "private-context-pack-secret";
    let messages = vec![chat(Role::User, secret)];
    let tools = vec![ToolSchema {
        name: "read".to_string(),
        description: format!("private description {secret}"),
        schema: Some(json!({"type": "object", "secret": secret})),
        group: "workspace".to_string(),
        dangerous: false,
    }];
    let model_options = BTreeMap::from([
        ("reasoning_effort".to_string(), json!("high")),
        ("private_value".to_string(), json!(secret)),
    ]);
    let input = ContextPackInput {
        system_sources: None,
        messages: messages.clone(),
        tools: tools.clone(),
        model_options: model_options.clone(),
        attachments: Vec::new(),
        work_state: None,
        checkpoints: Vec::new(),
        skills: Vec::new(),
        tool_manifests: Vec::new(),
        metadata: BTreeMap::from([(
            "execution".to_string(),
            json!({
                "mode": "opensandbox",
                "sandbox_id": "sandbox-test",
                "connection": {"token": secret},
            }),
        )]),
        todos: vec![ContextTodo::new(None, secret, "pending", "medium")],
        runtime_context: Some(format!("runtime {secret}")),
        sandbox_metadata: None,
        extra_items: Vec::new(),
    };
    let builder = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: true,
        ..ContextPackBuildOptions::default()
    }));
    let first = builder.build(input.clone());
    let second = builder.build(input);

    assert_eq!(first.schema_version, CONTEXT_PACK_SCHEMA_VERSION);
    assert_eq!(
        first.receipt.schema_version,
        CONTEXT_PACK_RECEIPT_SCHEMA_VERSION
    );
    assert_eq!(first.pack_hash, second.pack_hash);
    assert_eq!(first.provider_input_hash, second.provider_input_hash);
    assert_eq!(
        first.provider_input_hash,
        context_provider_input_hash(&messages, &tools, &model_options)
    );
    assert_eq!(first.receipt.message_count, 1);
    assert_eq!(first.receipt.tool_manifest_count, 1);
    assert_eq!(
        first.receipt.model_option_keys,
        vec!["private_value".to_string(), "reasoning_effort".to_string()]
    );
    assert_eq!(first.receipt.message_role_counts.get("user"), Some(&1));
    assert!(first.receipt.item_kind_counts.contains_key("sandbox"));
    assert!(first.receipt.item_kind_counts.contains_key("todo"));
    assert!(first.validate_provider_input().is_ok());

    let mut tampered = first.clone();
    tampered.messages[0].content.push_str("-tampered");
    let tamper_error = match tampered.validate_provider_input() {
        Ok(()) => panic!("tampered provider input unexpectedly passed validation"),
        Err(error) => error,
    };
    assert_eq!(tamper_error, "context pack provider input hash mismatch");

    let receipt = serde_json::to_string(&first.receipt)?;
    assert!(!receipt.contains(secret));
    assert!(!receipt.contains("private description"));
    assert!(!receipt.contains("connection"));
    Ok(())
}

#[test]
fn context_failure_and_performance_contracts_are_stable() {
    let failure = ContextFailure::new(
        ContextFailureCode::BudgetExceeded,
        "budget",
        "context is too large",
    );
    assert_eq!(failure.schema_version, CONTEXT_FAILURE_SCHEMA_VERSION);
    assert_eq!(failure.code, "context_budget_exceeded");
    assert!(!failure.retryable);
    assert!(failure.recoverable);
    assert_eq!(
        ContextFailureCode::Unavailable.as_str(),
        "context_unavailable"
    );
    assert_eq!(
        ContextFailureCode::ReceiptCorrupt.as_str(),
        "context_receipt_corrupt"
    );
    assert_eq!(
        ContextFailureCode::SourceDrift.as_str(),
        "context_source_drift"
    );
    assert_eq!(
        ContextFailureCode::ReplayUnsupported.as_str(),
        "context_replay_unsupported"
    );

    let mut performance = ContextPackPerformance::new();
    assert_eq!(
        performance.schema_version,
        CONTEXT_PERFORMANCE_SCHEMA_VERSION
    );
    assert_eq!(performance.status(), "ok");
    performance.build_us = openagent_core::CONTEXT_BUILD_WARN_US + 1;
    performance.provider_payload_serialize_us =
        openagent_core::CONTEXT_PROVIDER_PAYLOAD_SERIALIZE_WARN_US + 1;
    performance.provider_payload_bytes = openagent_core::CONTEXT_PROVIDER_PAYLOAD_WARN_BYTES + 1;
    performance.refresh_warnings();
    assert_eq!(performance.status(), "warning");
    assert_eq!(
        performance.warning_codes,
        vec![
            "context_build_slow",
            "provider_payload_serialize_slow",
            "provider_payload_large",
        ]
    );
}

#[test]
fn context_pack_long_session_build_stays_within_regression_budget() {
    let messages = (0..5_000)
        .map(|index| {
            chat(
                if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                &format!("long-session-message-{index} with deterministic context"),
            )
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    let pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        ..ContextPackBuildOptions::default()
    }))
    .build(ContextPackInput {
        messages,
        ..ContextPackInput::default()
    });
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "5k-message context build exceeded the 5s regression budget"
    );
    assert_eq!(pack.receipt.message_count, 5_000);
    assert_eq!(pack.messages.len(), 5_000);
}

#[test]
fn production_provider_boundaries_accept_only_verified_context_packs() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repository root");
    let cli_provider =
        fs::read_to_string(repo.join("cli/src/prompt/provider.rs")).expect("CLI provider adapter");
    assert!(cli_provider.contains("context_pack: &ContextPack"));
    assert!(cli_provider.contains("context_pack.validate_provider_input()?"));
    let cli_loop =
        fs::read_to_string(repo.join("cli/src/prompt/agent_loop.rs")).expect("CLI agent loop");
    assert!(cli_loop.contains("&context_pack,\n            Some(&mut on_provider_stream)"));

    let http_runtime = fs::read_to_string(repo.join("runtime/http/src/http_runtime.rs"))
        .expect("HTTP provider adapter");
    assert!(http_runtime.contains("context_pack: &'a ContextPack"));
    assert!(http_runtime.contains("context_pack.validate_provider_input()?"));
    assert!(!http_runtime.contains("build_run_prompt("));
    assert!(!http_runtime.contains("build_agent_system_prompt("));
    assert!(http_runtime.contains("ContextSystemSources"));
    assert!(http_runtime.contains("system_sources: Some(materialized.system_sources)"));
    assert!(http_runtime.contains("materialize_context_history(source_messages)"));
    assert!(!http_runtime.contains("fn runtime_take_context_work_state"));
    let cli_profile =
        fs::read_to_string(repo.join("cli/src/prompt/profile.rs")).expect("CLI profile context");
    assert!(!cli_profile.contains("build_agent_system_prompt("));
    assert!(cli_profile.contains("ContextSystemSources"));
    let cli_context =
        fs::read_to_string(repo.join("cli/src/prompt/context.rs")).expect("CLI context builder");
    assert!(cli_context.contains("system_sources: Some(system_sources)"));
    assert!(cli_context.contains("materialize_context_history("));
    assert!(!cli_context.contains("fn take_context_work_state"));
    let bridge_routes =
        fs::read_to_string(repo.join("runtime/http/src/bridge_routes.rs")).expect("Bridge routes");
    assert!(!bridge_routes.contains("pub fn build_run_prompt"));

    let provider_payload_files = rust_files_containing(
        repo,
        &[
            "build_openai_chat_payload(",
            "build_openai_responses_payload(",
            "build_anthropic_payload(",
        ],
    );
    assert_eq!(
        provider_payload_files,
        vec![
            "cli/src/prompt/provider.rs".to_string(),
            "runtime/http/src/http_runtime.rs".to_string(),
            "src/provider/src/provider.rs".to_string(),
        ],
        "provider payload assembly escaped the approved wire adapters"
    );
}

#[test]
fn context_pack_builder_is_the_only_system_source_materializer() -> Result<(), Box<dyn Error>> {
    let root = setup_goal6_fixture_named("system-sources")?;
    let workspace = root.join("repo/project/workspace");
    let profile_secret = "PROFILE_PROMPT_PRIVATE_BODY";
    let skill_secret = "PRELOADED_SKILL_PRIVATE_BODY";
    let sources = ContextSystemSources {
        profile_id: Some("build".to_string()),
        profile_mode: Some("primary".to_string()),
        profile_prompt: Some(profile_secret.to_string()),
        workspace_root: workspace.clone(),
        preloaded_skills: vec![SkillDocument {
            name: "code-review".to_string(),
            description: "Review code carefully".to_string(),
            location: workspace
                .join(".openagent/skills/code-review/SKILL.md")
                .to_string_lossy()
                .to_string(),
            directory: workspace
                .join(".openagent/skills/code-review")
                .to_string_lossy()
                .to_string(),
            metadata: BTreeMap::new(),
            score: None,
            content: skill_secret.to_string(),
        }],
        available_skills: vec![SkillInfo {
            name: "research".to_string(),
            description: "Research external sources".to_string(),
            location: workspace
                .join(".openagent/skills/research/SKILL.md")
                .to_string_lossy()
                .to_string(),
            directory: workspace
                .join(".openagent/skills/research")
                .to_string_lossy()
                .to_string(),
            metadata: BTreeMap::new(),
            score: None,
        }],
        legacy_system_sources: Vec::new(),
        include_instructions: true,
    };
    let build = |sources: ContextSystemSources| {
        ContextPackBuilder::new(Some(ContextPackBuildOptions {
            trace_only: false,
            ..ContextPackBuildOptions::default()
        }))
        .build(ContextPackInput {
            system_sources: Some(sources),
            messages: vec![chat(Role::User, "Inspect this project")],
            ..ContextPackInput::default()
        })
    };
    let first = build(sources.clone());

    assert_eq!(
        first
            .messages
            .iter()
            .filter(|message| message.role == Role::System)
            .count(),
        1
    );
    let system = &first.messages[0];
    assert_eq!(system.role, Role::System);
    assert!(system.content.contains(profile_secret));
    assert!(system.content.contains("Workspace rule"));
    assert!(system.content.contains(skill_secret));
    assert!(system.content.contains("research"));
    for kind in [
        "profile_prompt",
        "instruction",
        "skill_preloaded",
        "skill_available",
    ] {
        assert!(
            first
                .receipt
                .item_kind_counts
                .get(kind)
                .is_some_and(|count| *count > 0),
            "missing typed system source {kind}"
        );
    }
    assert!(
        first
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.kind.as_str(),
                    "profile_prompt" | "instruction" | "skill_preloaded" | "skill_available"
                )
            })
            .all(|item| item.delivery == ContextDelivery::TraceOnly)
    );
    let diagnostics = first
        .system_diagnostics
        .as_ref()
        .expect("system diagnostics");
    assert_eq!(diagnostics.profile_id.as_deref(), Some("build"));
    assert_eq!(
        diagnostics.preloaded_skill_names,
        vec!["code-review".to_string()]
    );
    assert!(diagnostics.instruction_count >= 2);
    let serialized_diagnostics = serde_json::to_string(diagnostics)?;
    assert!(!serialized_diagnostics.contains(profile_secret));
    assert!(!serialized_diagnostics.contains(skill_secret));
    assert!(!serialized_diagnostics.contains("Workspace rule"));
    first.validate_provider_input()?;

    fs::write(workspace.join("OPENAGENT.md"), "Workspace rule refreshed")?;
    let refreshed = build(sources);
    assert!(
        refreshed.messages[0]
            .content
            .contains("Workspace rule refreshed")
    );
    assert_ne!(
        refreshed
            .system_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.content_hash.as_str()),
        Some(diagnostics.content_hash.as_str())
    );
    Ok(())
}

#[test]
fn active_context_pack_normalizes_legacy_system_and_preserves_conversation_exactly() {
    let mut assistant = chat(Role::Assistant, "I will inspect the workspace.");
    assistant.metadata.insert(
        "tool_calls".to_string(),
        json!([{
            "id": "call-read",
            "type": "function",
            "function": {"name": "read", "arguments": "{\"path\":\"README.md\"}"}
        }]),
    );
    let messages = vec![
        chat(Role::System, "Follow project instructions."),
        chat(Role::User, "Inspect this project."),
        assistant,
        ChatMessage {
            role: Role::Tool,
            content: "workspace contents".to_string(),
            name: Some("read".to_string()),
            tool_call_id: Some("call-read".to_string()),
            metadata: BTreeMap::from([("status".to_string(), json!("completed"))]),
        },
    ];
    let tools = vec![ToolSchema {
        name: "read".to_string(),
        description: "Read a workspace file".to_string(),
        schema: Some(json!({"type": "object"})),
        group: "workspace".to_string(),
        dangerous: false,
    }];
    let model_options = BTreeMap::from([("reasoning_effort".to_string(), json!("high"))]);
    let pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        ..ContextPackBuildOptions::default()
    }))
    .build(ContextPackInput {
        messages: messages.clone(),
        tools: tools.clone(),
        model_options: model_options.clone(),
        ..ContextPackInput::default()
    });

    assert_eq!(pack.messages[0].role, Role::System);
    assert_eq!(pack.messages[0].content, messages[0].content);
    assert_eq!(&pack.messages[1..], &messages[1..]);
    assert_eq!(
        pack.messages
            .iter()
            .filter(|message| {
                message
                    .metadata
                    .get("dynamic_system_prompt")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count(),
        1
    );
    assert_eq!(
        pack.system_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.legacy_system_count),
        Some(1)
    );
    assert_eq!(pack.receipt.item_kind_counts.get("legacy_system"), Some(&1));
    assert_eq!(pack.tools, tools);
    assert_eq!(pack.model_options, model_options);
    assert_eq!(
        pack.provider_input_hash,
        context_provider_input_hash(&pack.messages, &pack.tools, &pack.model_options)
    );
}

#[test]
fn history_materialization_classifies_system_sources_and_remaps_positions()
-> Result<(), Box<dyn Error>> {
    let legacy_secret = "LEGACY_SYSTEM_PRIVATE_BODY";
    let mut profile = chat(Role::System, "STALE_PROFILE_PROMPT");
    profile
        .metadata
        .insert("agent_profile".to_string(), json!("build"));
    let mut boundary = chat(Role::System, "Resume from the compacted work state");
    boundary.metadata.extend(BTreeMap::from([
        ("kind".to_string(), json!("compaction_boundary")),
        ("message_id".to_string(), json!("compact-7")),
        ("compacted_until_message_id".to_string(), json!("message-6")),
        (
            "format".to_string(),
            json!("session_compaction_boundary_v1"),
        ),
    ]));
    let mut legacy = chat(Role::System, legacy_secret);
    legacy
        .metadata
        .insert("message_id".to_string(), json!("legacy-8"));
    let duplicate_legacy = legacy.clone();
    let user = chat(Role::User, "Continue the task");
    let raw_messages = vec![profile, boundary, legacy, duplicate_legacy, user.clone()];

    let history = materialize_context_history(raw_messages.clone());
    assert_eq!(history.messages, vec![user]);
    assert_eq!(history.discarded_profile_system_count, 1);
    assert_eq!(history.legacy_system_sources.len(), 2);
    assert_eq!(
        history.message_index_map,
        vec![None, None, None, None, Some(0)]
    );
    assert_eq!(history.message_position_map, vec![0, 0, 0, 0, 0, 1]);
    assert_eq!(
        history
            .work_state
            .as_ref()
            .map(|work_state| work_state.summary.as_str()),
        Some("Resume from the compacted work state")
    );

    let attachment = ContextAttachment::new(
        ContextAttachmentKind::File,
        Some("/workspace/task.md".to_string()),
        Some("task.md".to_string()),
        "text/markdown",
        12,
        "attachment body",
    )
    .with_source_message_index(4);
    let pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        ..ContextPackBuildOptions::default()
    }))
    .build(ContextPackInput {
        messages: raw_messages,
        attachments: vec![attachment],
        ..ContextPackInput::default()
    });

    assert_eq!(
        pack.messages
            .iter()
            .filter(|message| {
                message
                    .metadata
                    .get("dynamic_system_prompt")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count(),
        1
    );
    assert!(!pack.messages[0].content.contains("STALE_PROFILE_PROMPT"));
    assert_eq!(pack.messages[0].content.matches(legacy_secret).count(), 1);
    assert!(pack.messages.iter().any(|message| {
        message
            .content
            .contains("Resume from the compacted work state")
    }));
    assert_eq!(
        pack.messages.iter().find_map(|message| {
            message
                .metadata
                .get("context_attachment")
                .and_then(|attachment| attachment.get("source_message_index"))
                .and_then(Value::as_u64)
        }),
        Some(0)
    );
    assert_eq!(
        pack.system_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.legacy_system_count),
        Some(1)
    );
    assert_eq!(pack.receipt.item_kind_counts.get("legacy_system"), Some(&1));
    assert!(!serde_json::to_string(&pack.receipt)?.contains(legacy_secret));
    assert!(!serde_json::to_string(&pack.system_diagnostics)?.contains(legacy_secret));
    pack.validate_provider_input()?;
    Ok(())
}

#[test]
fn stable_prefix_partitions_messages_and_semantically_dedupes_static_context() {
    let mut shared_instruction = ContextItem::new(
        "instruction:workspace:a",
        "instruction",
        "workspace.instructions:a",
        "Shared project rule\nKeep tests deterministic.",
        100,
    );
    shared_instruction.pinned = true;
    shared_instruction.stable_prefix = true;
    let mut duplicate_instruction = ContextItem::new(
        "instruction:user:b",
        "instruction",
        "user.instructions:b",
        "Shared project rule\r\nKeep tests deterministic.   ",
        100,
    );
    duplicate_instruction.pinned = true;
    duplicate_instruction.stable_prefix = true;
    let input = ContextPackInput {
        messages: vec![
            chat(Role::System, "Stable profile prompt"),
            chat(Role::User, "DYNAMIC_REQUEST_ONE"),
        ],
        tools: vec![ToolSchema {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            schema: Some(json!({"type": "object"})),
            group: "workspace".to_string(),
            dangerous: false,
        }],
        work_state: Some(ContextWorkState {
            id: "dynamic-work".to_string(),
            summary: "DYNAMIC_WORK_ONE".to_string(),
            format: "structured_work_state".to_string(),
            source: "session.compaction".to_string(),
            message_position: Some(0),
            compacted_until_message_id: Some("msg-1".to_string()),
            metadata: BTreeMap::new(),
        }),
        extra_items: vec![shared_instruction, duplicate_instruction],
        ..ContextPackInput::default()
    };
    let builder = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        model_id: Some("prefix-model".to_string()),
        ..ContextPackBuildOptions::default()
    }));
    let first = builder.build(input.clone());
    assert_eq!(first.messages[0].content, "Stable profile prompt");
    assert!(first.messages[1].content.contains("Shared project rule"));
    assert!(first.messages[2].content.contains("DYNAMIC_WORK_ONE"));
    assert_eq!(first.messages[3].content, "DYNAMIC_REQUEST_ONE");
    let duplicate = first
        .trace
        .iter()
        .find(|entry| entry.item_id == "instruction:user:b")
        .expect("duplicate trace");
    assert!(!duplicate.included);
    assert_eq!(duplicate.drop_reason.as_deref(), Some("semantic_duplicate"));
    assert_eq!(
        duplicate.semantic_duplicate_of.as_deref(),
        Some("instruction:workspace:a")
    );
    assert_eq!(first.receipt.semantic_duplicate_count, 1);
    assert_eq!(
        first.receipt.drop_reason_counts.get("semantic_duplicate"),
        Some(&1)
    );
    assert!(first.stable_prefix.cache_eligible);
    assert_eq!(first.stable_prefix.message_count, 2);
    assert_eq!(first.stable_prefix.item_count, 2);
    assert_eq!(first.receipt.stable_prefix, first.stable_prefix);

    let mut dynamic_changed = input.clone();
    dynamic_changed.messages[1].content = "DYNAMIC_REQUEST_TWO".to_string();
    dynamic_changed
        .work_state
        .as_mut()
        .expect("work state")
        .summary = "DYNAMIC_WORK_TWO".to_string();
    let second = builder.build(dynamic_changed);
    assert_eq!(first.stable_prefix.hash, second.stable_prefix.hash);
    assert_ne!(first.provider_input_hash, second.provider_input_hash);

    let mut stable_changed = input;
    stable_changed.extra_items[0].content = "Changed project rule".to_string();
    let third = builder.build(stable_changed);
    assert_ne!(first.stable_prefix.hash, third.stable_prefix.hash);
}

#[test]
fn typed_skill_and_mcp_manifest_items_do_not_duplicate_provider_messages()
-> Result<(), Box<dyn Error>> {
    let secret = "private-skill-body";
    let skills = skill_context_items(
        &[SkillDocument {
            name: "review".to_string(),
            description: "Review changes".to_string(),
            location: "/workspace/.openagent/skills/review/SKILL.md".to_string(),
            directory: "/workspace/.openagent/skills/review".to_string(),
            metadata: BTreeMap::new(),
            score: None,
            content: secret.to_string(),
        }],
        &[SkillInfo {
            name: "research".to_string(),
            description: "Research sources".to_string(),
            location: "/workspace/.openagent/skills/research/SKILL.md".to_string(),
            directory: "/workspace/.openagent/skills/research".to_string(),
            metadata: BTreeMap::new(),
            score: None,
        }],
    );
    let tool = ToolSchema {
        name: "mcp_tool_docs_search".to_string(),
        description: "Search remote docs".to_string(),
        schema: Some(json!({"type": "object", "properties": {"query": {"type": "string"}}})),
        group: "remote-mcp".to_string(),
        dangerous: true,
    };
    let manifest = tool_manifest_context_item(
        "mcp_tool:docs:mcp_tool_docs_search",
        "mcp.server:docs",
        &tool,
        BTreeMap::from([
            ("server_name".to_string(), json!("docs")),
            ("original_name".to_string(), json!("search")),
        ]),
    );
    let messages = vec![chat(Role::User, "Find the answer")];
    let pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        ..ContextPackBuildOptions::default()
    }))
    .build(ContextPackInput {
        messages: messages.clone(),
        tools: vec![tool],
        skills,
        tool_manifests: vec![manifest],
        ..ContextPackInput::default()
    });

    assert_eq!(pack.messages, messages);
    assert_eq!(
        pack.receipt.item_kind_counts.get("skill_preloaded"),
        Some(&1)
    );
    assert_eq!(
        pack.receipt.item_kind_counts.get("skill_available"),
        Some(&1)
    );
    assert_eq!(
        pack.receipt.item_kind_counts.get("mcp_tool_manifest"),
        Some(&1)
    );
    assert_eq!(
        pack.receipt.item_delivery_counts.get("trace_only"),
        Some(&2)
    );
    assert_eq!(
        pack.receipt.item_delivery_counts.get("tool_manifest"),
        Some(&1)
    );
    assert!(
        pack.trace
            .iter()
            .any(|entry| entry.kind == "mcp_tool_manifest"
                && entry.delivery == ContextDelivery::ToolManifest)
    );
    assert!(!serde_json::to_string(&pack.receipt)?.contains(secret));
    Ok(())
}

#[test]
fn typed_attachment_is_stable_interleaved_and_redacted_from_receipt() -> Result<(), Box<dyn Error>>
{
    let secret = "private attachment body";
    let attachment = ContextAttachment::new(
        ContextAttachmentKind::File,
        Some("/workspace/design.md".to_string()),
        Some("design.md".to_string()),
        "text/markdown",
        secret.len() as u64,
        secret,
    )
    .with_source_message_index(1);
    let stable_id = attachment.id.clone();
    let pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        ..ContextPackBuildOptions::default()
    }))
    .build(ContextPackInput {
        system_sources: None,
        messages: vec![
            chat(Role::System, "Follow instructions"),
            chat(Role::User, "Review the attachment"),
            chat(Role::Assistant, "Previous answer"),
        ],
        attachments: vec![attachment],
        ..ContextPackInput::default()
    });

    assert_eq!(stable_id.len(), "att_".len() + 16);
    assert_eq!(
        pack.messages[2].metadata["context_attachment"]["included_content_bytes"],
        json!(secret.len())
    );
    assert_eq!(pack.messages.len(), 4);
    assert_eq!(pack.messages[1].content, "Review the attachment");
    assert_eq!(pack.messages[2].role, Role::User);
    assert!(pack.messages[2].content.contains(secret));
    assert_eq!(
        pack.messages[2].metadata["context_attachment"]["id"],
        json!(stable_id)
    );
    assert_eq!(pack.messages[3].content, "Previous answer");
    assert_eq!(
        pack.receipt.item_kind_counts.get("attachment_file"),
        Some(&1)
    );
    assert!(!serde_json::to_string(&pack.receipt)?.contains(secret));
    Ok(())
}

#[test]
fn typed_todo_checkpoint_and_work_state_preserve_order_and_redact_receipt()
-> Result<(), Box<dyn Error>> {
    let secret = "private work state detail";
    let work_state = ContextWorkState {
        id: "compact-msg-7".to_string(),
        summary: secret.to_string(),
        format: "structured_work_state".to_string(),
        source: "session.transcript.compaction".to_string(),
        message_position: Some(1),
        compacted_until_message_id: Some("msg-6".to_string()),
        metadata: BTreeMap::new(),
    };
    let todo = ContextTodo::new(
        Some("todo-context".to_string()),
        "Finish context migration",
        "in_progress",
        "high",
    );
    let checkpoint = ContextCheckpoint {
        id: "ckpt-8".to_string(),
        kind: "step_end".to_string(),
        run_id: "run-1".to_string(),
        timestamp_ms: 8,
        message_id: Some("msg-8".to_string()),
        part_id: None,
        step_index: Some(2),
        file_count: 12,
        total_bytes: 2048,
        restored: false,
        metadata: BTreeMap::new(),
    };
    let pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        ..ContextPackBuildOptions::default()
    }))
    .build(ContextPackInput {
        messages: vec![
            chat(Role::System, "System instructions"),
            chat(Role::User, "Continue the task"),
        ],
        work_state: Some(work_state),
        todos: vec![todo],
        checkpoints: vec![checkpoint],
        ..ContextPackInput::default()
    });

    assert_eq!(pack.messages.len(), 5);
    assert_eq!(pack.messages[0].content, "System instructions");
    assert!(pack.messages[1].content.contains(secret));
    assert!(
        pack.messages[2]
            .content
            .contains("Finish context migration")
    );
    assert!(pack.messages[3].content.contains("ckpt-8"));
    assert_eq!(pack.messages[4].content, "Continue the task");
    assert_eq!(pack.receipt.item_kind_counts.get("work_state"), Some(&1));
    assert_eq!(pack.receipt.item_kind_counts.get("todo"), Some(&1));
    assert_eq!(pack.receipt.item_kind_counts.get("checkpoint"), Some(&1));
    assert!(!serde_json::to_string(&pack.receipt)?.contains(secret));
    Ok(())
}

#[test]
fn model_aware_context_budget_drops_old_tool_output_and_is_deterministic()
-> Result<(), Box<dyn Error>> {
    let model = Model {
        id: "small-context-model".to_string(),
        provider_id: "openai".to_string(),
        name: "Small context model".to_string(),
        context_window: 480,
        max_output: 80,
        capabilities: ModelCapabilities::default(),
        pricing: ModelPricing::default(),
    };
    let options = openagent_core::context_pack_build_options_for_model(
        Some(&json!({
            "context_budget": {
                "input_safety_margin_tokens": 20,
                "bytes_per_token": 3
            }
        })),
        &model,
        false,
    )?;
    let input = ContextPackInput {
        messages: vec![
            chat(Role::System, "Always follow the project instructions."),
            chat(Role::User, "Earlier request"),
            ChatMessage {
                role: Role::Tool,
                content: format!("OLD_TOOL_OUTPUT {}", "large-result ".repeat(120)),
                name: Some("read".to_string()),
                tool_call_id: Some("call-old-read".to_string()),
                metadata: BTreeMap::new(),
            },
            chat(Role::User, "LATEST_USER_REQUEST"),
        ],
        tools: vec![ToolSchema {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            schema: Some(json!({"type": "object"})),
            group: "workspace".to_string(),
            dangerous: false,
        }],
        ..ContextPackInput::default()
    };
    let first = ContextPackBuilder::new(Some(options.clone())).build(input.clone());
    let second = ContextPackBuilder::new(Some(options)).build(input);

    assert_eq!(first.pack_hash, second.pack_hash);
    assert_eq!(first.budget.context_window, Some(480));
    assert_eq!(first.budget.reserved_output_tokens, 80);
    assert_eq!(first.budget.input_limit_tokens, Some(380));
    assert!(!first.budget.overflowed);
    assert!(first.estimated_input_tokens <= 380);
    assert!(
        first
            .messages
            .iter()
            .any(|message| message.role == Role::System)
    );
    assert!(
        first
            .messages
            .iter()
            .any(|message| message.content == "LATEST_USER_REQUEST")
    );
    assert!(
        first
            .messages
            .iter()
            .all(|message| !message.content.contains("OLD_TOOL_OUTPUT"))
    );
    let dropped = first
        .trace
        .iter()
        .find(|entry| entry.item_id == "tool_result:call-old-read")
        .expect("old tool trace");
    assert!(!dropped.included);
    assert_eq!(dropped.drop_reason.as_deref(), Some("model_context_budget"));
    assert_eq!(
        first.receipt.drop_reason_counts.get("model_context_budget"),
        Some(&1)
    );
    Ok(())
}

#[test]
fn required_context_is_source_aware_truncated_and_strict_error_remains_available()
-> Result<(), Box<dyn Error>> {
    let model = Model {
        id: "required-fit-model".to_string(),
        provider_id: "openai".to_string(),
        name: "Required fit model".to_string(),
        context_window: 3_200,
        max_output: 500,
        capabilities: ModelCapabilities::default(),
        pricing: ModelPricing::default(),
    };
    let options = openagent_core::context_pack_build_options_for_model(
        Some(&json!({
            "context_budget": {
                "strategy": "compact",
                "input_safety_margin_tokens": 300,
                "bytes_per_token": 3
            }
        })),
        &model,
        false,
    )?;
    let attachment = ContextAttachment::new(
        ContextAttachmentKind::File,
        Some("/workspace/large-design.md".to_string()),
        Some("large-design.md".to_string()),
        "text/markdown",
        60_000,
        format!(
            "ATTACHMENT_HEAD\n{}\nATTACHMENT_TAIL",
            "attachment-body ".repeat(3_000)
        ),
    )
    .with_source_message_index(1);
    let attachment_item_id = format!("attachment:{}:message:0", attachment.id);
    let input = ContextPackInput {
        messages: vec![
            chat(
                Role::System,
                &format!(
                    "SYSTEM_HEAD\n{}\nSYSTEM_TAIL",
                    "project-instruction ".repeat(3_000)
                ),
            ),
            chat(
                Role::User,
                &format!(
                    "LATEST_USER_HEAD\n{}\nLATEST_USER_TAIL",
                    "latest-request-detail ".repeat(3_000)
                ),
            ),
        ],
        attachments: vec![attachment],
        work_state: Some(ContextWorkState {
            id: "work-fit".to_string(),
            summary: format!(
                "WORK_STATE_HEAD\n{}\nWORK_STATE_TAIL",
                "completed-step ".repeat(3_000)
            ),
            format: "structured_work_state".to_string(),
            source: "session.compaction".to_string(),
            message_position: Some(0),
            compacted_until_message_id: Some("msg-before-fit".to_string()),
            metadata: BTreeMap::new(),
        }),
        todos: vec![ContextTodo::new(
            Some("todo-fit".to_string()),
            format!(
                "TODO_HEAD\n{}\nTODO_TAIL",
                "remaining-action ".repeat(3_000)
            ),
            "pending",
            "high",
        )],
        ..ContextPackInput::default()
    };

    let first = ContextPackBuilder::new(Some(options.clone())).build(input.clone());
    let second = ContextPackBuilder::new(Some(options)).build(input.clone());
    assert_eq!(first.pack_hash, second.pack_hash);
    assert_eq!(first.provider_input_hash, second.provider_input_hash);
    assert!(!first.budget.overflowed);
    assert!(
        first.estimated_input_tokens
            <= first
                .budget
                .input_limit_tokens
                .expect("input budget enabled")
    );
    let expected_ids = [
        "system:context_sources",
        "message:user:0",
        "work_state:work-fit",
        "todo:todo-fit",
        attachment_item_id.as_str(),
    ];
    for item_id in expected_ids {
        let entry = first
            .trace
            .iter()
            .find(|entry| entry.item_id == item_id)
            .unwrap_or_else(|| panic!("missing trace for {item_id}"));
        assert!(entry.included, "{item_id} was dropped");
        assert!(entry.truncated, "{item_id} was not fitted");
        assert_eq!(
            entry.truncation_reason.as_deref(),
            Some("required_context_budget")
        );
        assert!(
            entry
                .original_token_estimate
                .is_some_and(|original| original > entry.token_estimate)
        );
    }
    assert_eq!(first.receipt.truncated_item_count, 5);
    assert_eq!(
        first
            .receipt
            .truncation_reason_counts
            .get("required_context_budget"),
        Some(&5)
    );
    assert!(
        first
            .messages
            .iter()
            .filter(|message| message
                .content
                .contains("context truncated to fit model budget"))
            .count()
            >= 5
    );
    for marker in [
        "SYSTEM_HEAD",
        "SYSTEM_TAIL",
        "LATEST_USER_HEAD",
        "LATEST_USER_TAIL",
        "ATTACHMENT_HEAD",
        "ATTACHMENT_TAIL",
        "WORK_STATE_HEAD",
        "WORK_STATE_TAIL",
        "TODO_HEAD",
        "TODO_TAIL",
    ] {
        assert!(
            first
                .messages
                .iter()
                .any(|message| message.content.contains(marker)),
            "missing retained marker {marker}"
        );
    }

    let strict_options = openagent_core::context_pack_build_options_for_model(
        Some(&json!({
            "context_budget": {
                "strategy": "error",
                "input_safety_margin_tokens": 300,
                "bytes_per_token": 3
            }
        })),
        &model,
        false,
    )?;
    let strict = ContextPackBuilder::new(Some(strict_options)).build(input);
    assert!(strict.budget.overflowed);
    assert_eq!(strict.receipt.truncated_item_count, 0);
    assert!(
        strict
            .receipt
            .drop_reason_counts
            .get("required_budget_exhausted")
            .is_some_and(|count| *count > 0)
    );
    Ok(())
}

#[test]
fn permission_manager_uses_last_matching_rule_and_payload_patterns() -> Result<(), Box<dyn Error>> {
    let mut manager = PermissionManager::new();
    manager.set_ruleset(PermissionRuleset::None);
    manager.add_rule(permission_rule(
        "skill",
        PermissionAction::Allow,
        Some("code-*"),
    ));
    manager.add_rule(permission_rule(
        "skill",
        PermissionAction::Deny,
        Some("code-secret"),
    ));

    assert_eq!(
        manager.decide(&json!({"name": "skill", "input": {"name": "code-review"}})),
        PermissionAction::Allow
    );
    assert_eq!(
        manager.decide(&json!({"name": "skill", "input": {"name": "code-secret"}})),
        PermissionAction::Deny
    );
    let denied = manager
        .check(&json!({"name": "skill", "input": {"name": "code-secret"}}))
        .expect_err("deny must block in check");
    assert!(denied.contains("Permission denied"));
    assert_eq!(pattern_for(&json!({"file_path": "a.txt"})), "a.txt");
    assert_eq!(
        manager.decide(&json!({"name": "bash", "input": {"command": "echo hi"}})),
        PermissionAction::Deny
    );
    Ok(())
}

#[test]
fn instruction_loader_and_skill_registry_cover_filesystem_workflows() -> Result<(), Box<dyn Error>>
{
    let root = setup_goal6_fixture_named("instruction")?;
    let workspace = root.join("repo/project/workspace");
    let user_dir = root.join("user");

    let instructions = InstructionContextLoader::new(
        &workspace,
        Some(InstructionLoadOptions {
            max_file_bytes: 8,
            max_total_bytes: 64,
            user_config_dir: Some(user_dir),
            ..InstructionLoadOptions::default()
        }),
    )
    .load();
    assert_eq!(instructions.items[0].display_path, "OPENAGENT.md");
    assert_eq!(instructions.items[0].content, "Workspac");
    assert!(instructions.truncated);
    assert!(
        instructions
            .issues
            .contains(&"truncated:OPENAGENT.md".to_string())
    );

    let registry = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(root.join("home")),
        SkillRegistryOptions {
            include_builtin_skills: false,
        },
    );
    let report = registry.report(Some("review"), Some(5));
    assert_eq!(report.loaded_count, 2);
    assert_eq!(report.invalid_count, 1);
    assert_eq!(report.duplicate_count, 1);
    assert_eq!(report.skills[0].name, "code-review");
    assert_eq!(
        registry.search("external evidence", None)[0].name,
        "research"
    );
    Ok(())
}

#[test]
fn skill_registry_discovers_builtin_skills_and_workspace_overrides() -> Result<(), Box<dyn Error>> {
    let root = setup_goal6_fixture_named("builtin-skills")?;
    let workspace = root.join("repo/project/workspace");
    let empty_home = root.join("empty-home");
    fs::create_dir_all(&empty_home)?;

    let builtins = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(&empty_home),
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    );
    assert!(
        builtins
            .get("openai-docs")
            .is_some_and(|skill| skill.location.contains("skill/openagent"))
    );

    write_skill(
        &workspace,
        ".openagent/skills/openai-docs/SKILL.md",
        "openai-docs",
        "Workspace override for OpenAI docs",
        "Workspace override wins.",
    )?;
    let overridden = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(&empty_home),
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    );
    let loaded = overridden
        .get("openai-docs")
        .ok_or("missing overridden openai-docs skill")?;
    assert!(loaded.location.contains(".openagent/skills/openai-docs"));
    assert_eq!(loaded.content.trim(), "Workspace override wins.");

    let disabled = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(&empty_home),
        SkillRegistryOptions {
            include_builtin_skills: false,
        },
    );
    assert!(
        disabled
            .all()
            .iter()
            .all(|skill| !skill.location.contains("skill/openagent"))
    );
    Ok(())
}

fn core_context_policy_fixture() -> Result<Value, Box<dyn Error>> {
    let root = setup_goal6_fixture()?;
    let workspace = root.join("repo/project/workspace");
    let user_dir = root.join("user");

    let model = Model {
        id: "context-fixture".to_string(),
        provider_id: "fixture".to_string(),
        name: "Context Fixture".to_string(),
        context_window: 96,
        max_output: 24,
        capabilities: Default::default(),
        pricing: Default::default(),
    };
    let budget_messages = vec![
        ChatMessage {
            role: Role::User,
            content: "find matches".to_string(),
            name: None,
            tool_call_id: None,
            metadata: BTreeMap::new(),
        },
        ChatMessage {
            role: Role::Tool,
            content: "x".repeat(1200),
            name: Some("code_search".to_string()),
            tool_call_id: None,
            metadata: BTreeMap::new(),
        },
    ];
    let budget_tools = vec![ToolSchema {
        name: "large_tool".to_string(),
        description: "A".repeat(120),
        schema: Some(json!({
            "type": "object",
            "properties": {"query": {"type": "string", "description": "B".repeat(80)}},
        })),
        group: "default".to_string(),
        dangerous: false,
    }];
    let budget_options = json!({"context_budget": {"strategy": "compact", "bytes_per_token": 4}});
    let budget_result = check_context_budget(
        Some("You are helpful."),
        &budget_messages,
        &budget_tools,
        Some(&model),
        Some(&budget_options),
        "goal6",
    )?
    .ok_or("budget result missing")?;

    let invalid_strategy = load_context_budget_options(
        Some(&json!({"context_budget": {"strategy": ""}})),
        Some(&model),
    )
    .err()
    .ok_or("invalid strategy unexpectedly passed")?;
    let invalid_compaction =
        load_context_budget_options(Some(&json!({"compaction": {"auto": "yes"}})), Some(&model))
            .err()
            .ok_or("invalid compaction unexpectedly passed")?;

    let context_pack = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        token_budget: Some(150),
        bytes_per_token: 4,
        trace_only: true,
        model_id: Some("fixture-model".to_string()),
        context_window: Some(200),
        reserved_output_tokens: 50,
        fit_required_context: true,
    }))
    .build(ContextPackInput {
        system_sources: None,
        messages: vec![
            chat(Role::User, "old request"),
            ChatMessage {
                role: Role::Tool,
                content: "grep preview".to_string(),
                name: Some("grep".to_string()),
                tool_call_id: Some("call-grep".to_string()),
                metadata: BTreeMap::new(),
            },
            chat(Role::User, "new request"),
        ],
        tools: Vec::new(),
        model_options: BTreeMap::new(),
        attachments: Vec::new(),
        work_state: None,
        checkpoints: Vec::new(),
        skills: Vec::new(),
        tool_manifests: Vec::new(),
        metadata: BTreeMap::from([
            (
                "context_compaction".to_string(),
                json!({
                    "schema_version": 1,
                    "format": "structured_work_state",
                    "state": {"task": "Continue Rust rewrite", "next_steps": ["Port context"]},
                    "summary": "ignored",
                    "compacted_until": 2,
                    "updated_at": 1781841000000u64,
                }),
            ),
            (
                "execution".to_string(),
                json!({
                    "mode": "opensandbox",
                    "sandbox_id": "sbx_fixture",
                    "remote_workdir": "/workspace/project",
                    "connection": {"token": "secret"},
                }),
            ),
        ]),
        todos: vec![ContextTodo::new(
            Some("todo-context".to_string()),
            "port context",
            "in_progress",
            "high",
        )],
        runtime_context: Some("[Runtime]\nGoal 6 fixture".to_string()),
        sandbox_metadata: None,
        extra_items: vec![
            ContextItem::new("diag", "diagnostic", "fixture", "low", 1),
            ContextItem::new("diag", "diagnostic", "fixture", "high", 9),
        ],
    });

    let instructions = InstructionContextLoader::new(
        &workspace,
        Some(InstructionLoadOptions {
            max_file_bytes: 8,
            max_total_bytes: 64,
            user_config_dir: Some(user_dir),
            ..InstructionLoadOptions::default()
        }),
    )
    .load();
    let instruction_context_items = instructions.to_context_items();

    let registry = SkillRegistry::new_with_options(
        Some(&workspace),
        None,
        Some(root.join("home")),
        SkillRegistryOptions {
            include_builtin_skills: false,
        },
    );
    let report = registry.report(Some("review"), Some(5));
    let loaded = registry
        .get("code-review")
        .ok_or("missing code-review skill")?;

    let payload = json!({
        "schema_version": 1,
        "permission": permission_decisions()?,
        "context_budget": {
            "config": to_value(load_context_budget_options(
                Some(&json!({
                    "compaction": {"auto": false, "prune": false, "reserved": 16},
                    "context_budget": {"strategy": "compact", "input_safety_margin_tokens": 8},
                })),
                Some(&model),
            )?)?,
            "result": to_value(&budget_result)?,
            "error": format_context_budget_error(&budget_result),
            "invalid_strategy": invalid_strategy,
            "invalid_compaction": invalid_compaction,
        },
        "context_pack": {
            "estimated_input_tokens": context_pack.estimated_input_tokens,
            "budget": to_value(&context_pack.budget)?,
            "stable_prefix": to_value(&context_pack.stable_prefix)?,
            "items": context_pack.items.iter().map(context_item_fixture).collect::<Vec<_>>(),
            "trace": to_value(&context_pack.trace)?,
            "estimate_text_tokens": estimate_text_tokens("abcd", 3),
        },
        "instructions": {
            "total_bytes": instructions.total_bytes,
            "truncated": instructions.truncated,
            "issues": instructions.issues,
            "items": instructions.items.iter().map(instruction_item_fixture).collect::<Vec<_>>(),
            "context_items": instruction_context_items.iter().map(instruction_context_item_fixture).collect::<Vec<_>>(),
        },
        "skills": {
            "report": {
                "skill_count": report.skills.len(),
                "loaded_count": report.loaded_count,
                "scanned_files": report.scanned_files,
                "invalid_count": report.invalid_count,
                "duplicate_count": report.duplicate_count,
                "skills": to_value(&report.skills)?,
                "issues": report.issues.iter().map(skill_issue_summary).collect::<Vec<_>>(),
            },
            "loaded": to_value(&loaded)?,
            "search_all": to_value(registry.search("external evidence", None))?,
        },
    });
    Ok(scrub_fixture_root(payload, &root))
}

fn permission_decisions() -> Result<Value, Box<dyn Error>> {
    let mut readonly = PermissionManager::new();
    readonly.set_ruleset(PermissionRuleset::Readonly);
    let mut plan_only = PermissionManager::new();
    plan_only.set_ruleset(PermissionRuleset::PlanOnly);
    let mut custom = PermissionManager::new();
    custom.set_ruleset(PermissionRuleset::None);
    custom.add_rule(permission_rule(
        "skill",
        PermissionAction::Allow,
        Some("code-review"),
    ));
    Ok(json!({
        "readonly_write": readonly.decide(&json!({"name": "write", "input": {"file_path": "a.txt", "content": "x"}})),
        "readonly_ls": readonly.decide(&json!({"name": "ls", "input": {}})),
        "readonly_skill": readonly.decide(&json!({"name": "skill", "input": {"name": "code-review"}})),
        "readonly_todowrite": readonly.decide(&json!({"name": "todowrite", "input": {"todos": []}})),
        "plan_only_todowrite": plan_only.decide(&json!({"name": "todowrite", "input": {"todos": []}})),
        "custom_skill": custom.decide(&json!({"name": "skill", "input": {"name": "code-review"}})),
        "pattern_for_file": pattern_for(&json!({"file_path": "src/core.rs", "command": "ignored"})),
        "pattern_for_name": pattern_for(&json!({"name": "code-review"})),
        "pattern_for_json": pattern_for(&json!({"b": 2, "a": 1})),
    }))
}

fn setup_goal6_fixture() -> Result<PathBuf, Box<dyn Error>> {
    setup_goal6_fixture_at(PathBuf::from("/tmp/openagent-rust-rewrite-fixture-goal6"))
}

fn setup_goal6_fixture_named(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    setup_goal6_fixture_at(PathBuf::from(format!(
        "/tmp/openagent-rust-rewrite-fixture-goal6-{name}"
    )))
}

fn setup_goal6_fixture_at(root: PathBuf) -> Result<PathBuf, Box<dyn Error>> {
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let workspace = root.join("repo/project/workspace");
    let user_dir = root.join("user");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&user_dir)?;
    fs::write(root.join("repo/AGENTS.md"), "Parent instruction")?;
    fs::write(workspace.join("OPENAGENT.md"), "Workspace rule")?;
    fs::create_dir_all(workspace.join(".openagent/rules"))?;
    fs::write(workspace.join(".openagent/rules/b.md"), "Rule B")?;
    fs::write(workspace.join(".openagent/rules/a.md"), "Rule A")?;
    fs::write(user_dir.join("OPENAGENT.md"), "User instruction")?;

    write_skill(
        &workspace,
        ".openagent/skills/code-review/SKILL.md",
        "code-review",
        "Review code carefully",
        "Inspect diffs and tests.",
    )?;
    write_skill(
        &workspace,
        ".openagent/skills/research/SKILL.md",
        "research",
        "Research external sources",
        "Collect evidence.",
    )?;
    write_skill(
        &workspace,
        ".claude/skills/code-review/SKILL.md",
        "code-review",
        "duplicate",
        "Duplicate should not win.",
    )?;
    let broken = workspace.join(".openagent/skills/broken/SKILL.md");
    if let Some(parent) = broken.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(broken, "# no frontmatter\n")?;
    Ok(root)
}

fn write_skill(
    base: &Path,
    relative: &str,
    name: &str,
    description: &str,
    body: &str,
) -> Result<(), Box<dyn Error>> {
    let path = base.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )?;
    Ok(())
}

fn chat(role: Role, content: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: content.to_string(),
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::new(),
    }
}

fn rust_files_containing(repo: &Path, needles: &[&str]) -> Vec<String> {
    let mut pending = ["cli/src", "runtime/http/src", "src/provider/src"]
        .into_iter()
        .map(|path| repo.join(path))
        .collect::<Vec<_>>();
    let mut matches = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                fs::read_dir(&path)
                    .expect("read production source directory")
                    .flatten()
                    .map(|entry| entry.path()),
            );
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read production Rust source");
        if needles.iter().any(|needle| source.contains(needle)) {
            matches.push(
                path.strip_prefix(repo)
                    .expect("source under repository")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    matches.sort();
    matches.dedup();
    matches
}

fn context_item_fixture(item: &ContextItem) -> Value {
    json!({
        "id": item.id,
        "kind": item.kind,
        "source": item.source,
        "content": item.content,
        "priority": item.priority,
        "token_estimate": item.token_estimate,
        "pinned": item.pinned,
        "stable_prefix": item.stable_prefix,
        "metadata": item.metadata,
    })
}

fn instruction_item_fixture(item: &openagent_core::InstructionItem) -> Value {
    json!({
        "display_path": item.display_path,
        "source": item.source,
        "scope": item.scope,
        "content": item.content,
        "bytes_read": item.bytes_read,
        "truncated": item.truncated,
    })
}

fn instruction_context_item_fixture(item: &ContextItem) -> Value {
    json!({
        "kind": item.kind,
        "source": item.source,
        "content": item.content,
        "priority": item.priority,
        "pinned": item.pinned,
        "stable_prefix": item.stable_prefix,
        "metadata": item.metadata,
    })
}

fn skill_issue_summary(issue: &openagent_core::SkillIssue) -> Value {
    json!({
        "kind": issue.kind,
        "path": Path::new(&issue.path).file_name().and_then(|name| name.to_str()).unwrap_or_default(),
        "duplicate_of": issue.duplicate_of.as_ref().and_then(|path| {
            Path::new(path).file_name().and_then(|name| name.to_str()).map(str::to_string)
        }),
    })
}

fn scrub_fixture_root(value: Value, root: &Path) -> Value {
    let stable = "/tmp/openagent-rust-rewrite-fixture-goal6";
    let mut replacements = vec![(root.to_string_lossy().to_string(), stable.to_string())];
    if let Ok(resolved) = root.canonicalize() {
        replacements.push((resolved.to_string_lossy().to_string(), stable.to_string()));
    }
    replacements.push((format!("/private{stable}"), stable.to_string()));
    scrub_value(value, &replacements)
}

fn scrub_value(value: Value, replacements: &[(String, String)]) -> Value {
    match value {
        Value::String(text) => {
            let mut result = text;
            for (needle, replacement) in replacements {
                result = result.replace(needle, replacement);
            }
            Value::String(result)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| scrub_value(item, replacements))
                .collect(),
        ),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .map(|(key, value)| (key, scrub_value(value, replacements)))
                .collect(),
        ),
        other => other,
    }
}

fn to_value<T: Serialize>(value: T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value)
}

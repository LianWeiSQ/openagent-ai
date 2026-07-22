use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::PathBuf,
};

use openagent_core::{
    ContextMicroCompactionOptions, ContextPackBuildOptions, ContextPackBuilder, ContextPackInput,
    context_work_state_from_epoch,
};
use openagent_eval::{
    CONTEXT_COMPACTION_CORPUS_SCHEMA_VERSION, ContextCompactionContinuity,
    ContextCompactionEvalCorpus, ContextCompactionEvalObservation, ContextCompactionEvalRubric,
    ExplorationQualityObservation, ExplorationQualityRubric, ExplorationToolCall,
    compare_context_compaction, compare_exploration_quality,
    context_compaction_observation_from_packs, eval_integrations_fixture,
    harbor_normalized_model_name, harbor_timeout_seconds, score_context_compaction,
    score_exploration_quality, terminal_bench_extract_returncode, terminal_bench_failure_mode,
};
use openagent_protocol::{
    ChatMessage, ContextEpoch, Role, SemanticAnchor, SemanticAnchorAuthority, SemanticAnchorKind,
    SemanticAnchorRegistry, SemanticAnchorScope, WorkState,
};
use serde_json::{Value, json};

#[test]
fn eval_integrations_fixture_matches_legacy_oracle() -> Result<(), Box<dyn Error>> {
    let fixture = read_fixture()?;
    assert_eq!(eval_integrations_fixture(), fixture);
    Ok(())
}

#[test]
fn benchmark_adapter_helpers_cover_edge_cases() {
    let (returncode, cleaned) = terminal_bench_extract_returncode(
        "body\n__OPENAGENT_TBENCH_EXIT_x__-9\n",
        "__OPENAGENT_TBENCH_EXIT_x__",
    );
    assert_eq!(returncode, -9);
    assert_eq!(cleaned, "body");
    assert_eq!(
        terminal_bench_failure_mode("context length exceeded"),
        "context_length_exceeded"
    );
    assert_eq!(harbor_timeout_seconds(5200), 6);
    assert_eq!(
        harbor_normalized_model_name(Some("openai-compatible/gpt-test")),
        Some("gpt-test".to_string())
    );
}

#[test]
fn exploration_quality_gate_detects_shallow_repository_answers() {
    let rubric = ExplorationQualityRubric {
        case_id: "repository-audit".to_string(),
        required_context_kinds: ["attachment_file", "instruction", "todo"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        required_available_tools: ["grep", "read"].into_iter().map(str::to_string).collect(),
        required_files: ["Cargo.toml", "src/core.rs"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        required_tools_used: ["grep", "read"].into_iter().map(str::to_string).collect(),
        required_answer_terms: ["contextpackbuilder", "provider boundary"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        forbidden_tools: ["write"].into_iter().map(str::to_string).collect(),
        max_failed_tool_calls: 0,
        max_duplicate_tool_calls: 0,
        minimum_score: 100.0,
    };
    let complete = ExplorationQualityObservation {
        case_id: "repository-audit".to_string(),
        completed: true,
        context_item_kinds: ["attachment_file", "instruction", "todo"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        available_tools: ["grep", "read"].into_iter().map(str::to_string).collect(),
        explored_files: ["Cargo.toml", "src/core.rs"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        tool_calls: vec![
            ExplorationToolCall {
                call_id: "grep".to_string(),
                name: "grep".to_string(),
                target: Some("ContextPackBuilder".to_string()),
                status: "completed".to_string(),
            },
            ExplorationToolCall {
                call_id: "read".to_string(),
                name: "read".to_string(),
                target: Some("src/core.rs".to_string()),
                status: "completed".to_string(),
            },
        ],
        final_answer: "ContextPackBuilder owns the provider boundary.".to_string(),
    };
    let baseline = score_exploration_quality(&rubric, &complete);
    assert!(baseline.passed);
    assert_eq!(baseline.score, 100.0);

    let mut shallow = complete;
    shallow.explored_files.remove("src/core.rs");
    shallow.final_answer = "The project seems fine.".to_string();
    shallow.tool_calls[1].status = "failed".to_string();
    let current = score_exploration_quality(&rubric, &shallow);
    assert!(!current.passed);
    assert!(current.score < baseline.score);
    assert_eq!(current.failed_tool_calls, 1);
    assert_eq!(current.missing_files, vec!["src/core.rs"]);
    assert_eq!(
        current.missing_answer_terms,
        vec!["contextpackbuilder", "provider boundary"]
    );

    let comparison = compare_exploration_quality(&baseline, &current, 0.0);
    assert!(!comparison.passed);
    assert!(
        comparison
            .regressions
            .iter()
            .any(|reason| reason.contains("file_coverage regressed"))
    );
}

#[test]
fn context_compaction_eval_contract_matches_versioned_golden() -> Result<(), Box<dyn Error>> {
    let actual = context_compaction_golden_payload();
    let expected: Value = serde_json::from_str(include_str!(
        "../../tests/golden/rust_rewrite/context_compaction_eval.json"
    ))?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn context_compaction_eval_rejects_invalid_contracts() {
    let rubric = compaction_rubric(&[]);
    let duplicate_corpus = ContextCompactionEvalCorpus {
        schema_version: CONTEXT_COMPACTION_CORPUS_SCHEMA_VERSION.to_string(),
        cases: vec![rubric.clone(), rubric.clone()],
    };
    assert_eq!(
        duplicate_corpus.validate(),
        Err(format!(
            "duplicate context compaction eval case: {}",
            rubric.case_id
        ))
    );

    let observation = ContextCompactionEvalObservation {
        schema_version: "openagent.eval.context_compaction.invalid".to_string(),
        case_id: "different-case".to_string(),
        ..ContextCompactionEvalObservation::default()
    };
    let result = score_context_compaction(&rubric, &observation);
    assert!(!result.passed);
    assert!(
        result
            .failure_reasons
            .iter()
            .any(|reason| reason.contains("unsupported context compaction observation schema"))
    );
    assert!(
        result
            .failure_reasons
            .iter()
            .any(|reason| reason.contains("context compaction case mismatch"))
    );
}

#[test]
fn long_session_compaction_preserves_semantics_and_rebuilds_identically() {
    let mut source_messages = Vec::new();
    for turn in 0..10 {
        source_messages.push(chat(
            Role::User,
            &format!("Historical investigation request {turn} for ContextPackBuilder"),
        ));
        source_messages.push(chat(
            Role::Assistant,
            &format!(
                "RAW_TOOL_NOISE_SENTINEL turn={turn} {}",
                "unimportant historical output ".repeat(180)
            ),
        ));
    }
    let before = ContextPackBuilder::new(Some(ContextPackBuildOptions {
        trace_only: false,
        micro_compaction: ContextMicroCompactionOptions {
            enabled: false,
            ..ContextMicroCompactionOptions::default()
        },
        ..ContextPackBuildOptions::default()
    }))
    .build(ContextPackInput {
        messages: source_messages.clone(),
        ..ContextPackInput::default()
    });

    let anchors = vec![
        SemanticAnchor::new(
            SemanticAnchorKind::Goal,
            "primary",
            "Finish the ContextPackBuilder compact eval suite",
            SemanticAnchorAuthority::ContextEpoch,
            SemanticAnchorScope::Session,
            "eval.fixture",
        ),
        SemanticAnchor::new(
            SemanticAnchorKind::Constraint,
            "provider-boundary",
            "Keep ContextPackBuilder as the only provider boundary",
            SemanticAnchorAuthority::ContextEpoch,
            SemanticAnchorScope::Session,
            "eval.fixture",
        ),
        SemanticAnchor::new(
            SemanticAnchorKind::Decision,
            "allocator",
            "Use the layered budget allocator",
            SemanticAnchorAuthority::ContextEpoch,
            SemanticAnchorScope::Session,
            "eval.fixture",
        ),
        SemanticAnchor::new(
            SemanticAnchorKind::File,
            "src/core.rs",
            "src/core.rs owns the context assembly boundary",
            SemanticAnchorAuthority::ContextEpoch,
            SemanticAnchorScope::Session,
            "eval.fixture",
        ),
        SemanticAnchor::new(
            SemanticAnchorKind::Blocker,
            "recovery",
            "Replay and restart consistency remain release blockers",
            SemanticAnchorAuthority::ContextEpoch,
            SemanticAnchorScope::Session,
            "eval.fixture",
        ),
        SemanticAnchor::new(
            SemanticAnchorKind::NextStep,
            "verify",
            "Verify replay and restart hashes",
            SemanticAnchorAuthority::ContextEpoch,
            SemanticAnchorScope::Epoch,
            "eval.fixture",
        ),
    ];
    let registry = SemanticAnchorRegistry::build(anchors.clone());
    let state = WorkState {
        task: "Finish the ContextPackBuilder compact eval suite".to_string(),
        constraints: vec!["Keep one provider boundary".to_string()],
        decisions: vec!["Use the layered budget allocator".to_string()],
        files: vec![openagent_protocol::WorkStateFile {
            path: "src/core.rs".to_string(),
            status: "modified".to_string(),
            note: "context assembly boundary".to_string(),
        }],
        blockers: vec!["Replay and restart consistency".to_string()],
        next_steps: vec!["Verify replay and restart hashes".to_string()],
        ..WorkState::default()
    };
    let epoch = ContextEpoch::manual(
        "epoch_eval_2",
        "session_eval",
        "run_eval",
        2,
        source_messages.len() as u64,
        Some("message_19".to_string()),
        openagent_protocol::render_work_state(&state),
    )
    .into_automatic(
        "history_budget_pressure",
        before.pack_hash.clone(),
        4,
        180,
        state,
    )
    .with_anchor_registry(registry.clone());
    assert!(epoch.validate().is_ok());
    let work_state = context_work_state_from_epoch(&epoch, 0).expect("typed epoch work state");

    let mut tool_call = chat(
        Role::Assistant,
        "Inspect the current context implementation.",
    );
    tool_call.metadata.insert(
        "tool_calls".to_string(),
        json!([{"id":"call_eval_read","type":"function","function":{"name":"read","arguments":"{\"path\":\"src/core.rs\"}"}}]),
    );
    let tool_result = ChatMessage {
        role: Role::Tool,
        content: "ContextPackBuilder and layered budget allocator are present.".to_string(),
        name: Some("read".to_string()),
        tool_call_id: Some("call_eval_read".to_string()),
        metadata: BTreeMap::new(),
    };
    let after_input = ContextPackInput {
        messages: vec![
            tool_call,
            tool_result,
            chat(Role::User, "Verify replay and restart consistency."),
        ],
        work_state: Some(work_state),
        ..ContextPackInput::default()
    };
    let after_options = ContextPackBuildOptions {
        token_budget: Some(2_200),
        bytes_per_token: 3,
        trace_only: false,
        model_id: Some("compact-eval-model".to_string()),
        context_window: Some(2_600),
        reserved_output_tokens: 400,
        ..ContextPackBuildOptions::default()
    };
    let after = ContextPackBuilder::new(Some(after_options.clone())).build(after_input.clone());
    let replay = ContextPackBuilder::new(Some(after_options.clone())).build(after_input.clone());
    let restart = ContextPackBuilder::new(Some(after_options)).build(after_input);

    let rubric = compaction_rubric(&anchors);
    let observation = context_compaction_observation_from_packs(
        &rubric.case_id,
        &before,
        &after,
        &rubric.required_terms,
        &rubric.forbidden_terms,
        ContextCompactionContinuity {
            typed_epoch_count: 1,
            epoch_parent_chain_valid: epoch.validate().is_ok(),
            ledger_preserved: source_messages.len() == 20,
            replay_pack_hash_match: replay.pack_hash == after.pack_hash,
            replay_anchor_registry_match: replay.semantic_anchor_registry.registry_hash
                == after.semantic_anchor_registry.registry_hash,
            restart_pack_hash_match: restart.pack_hash == after.pack_hash,
            restart_anchor_registry_match: restart.semantic_anchor_registry.registry_hash
                == after.semantic_anchor_registry.registry_hash,
        },
    );
    let result = score_context_compaction(&rubric, &observation);

    assert!(result.passed, "{}", result.failure_reasons.join("; "));
    assert_eq!(result.score, 100.0);
    assert!(result.token_savings_ratio >= 0.65);
    assert_eq!(observation.required_drop_count, 0);
    assert_eq!(observation.split_tool_group_count, 0);
    assert!(
        !after
            .messages
            .iter()
            .any(|message| { message.content.contains("RAW_TOOL_NOISE_SENTINEL") })
    );
    assert!(after.validate_provider_input().is_ok());
}

fn context_compaction_golden_payload() -> Value {
    let anchors = [
        SemanticAnchor::new(
            SemanticAnchorKind::Goal,
            "primary",
            "golden goal",
            SemanticAnchorAuthority::Explicit,
            SemanticAnchorScope::Session,
            "eval.golden",
        ),
        SemanticAnchor::new(
            SemanticAnchorKind::Constraint,
            "boundary",
            "golden constraint",
            SemanticAnchorAuthority::Explicit,
            SemanticAnchorScope::Session,
            "eval.golden",
        ),
    ];
    let rubric = ContextCompactionEvalRubric {
        case_id: "long-session-golden".to_string(),
        description: "Long engineering session retains continuation semantics".to_string(),
        required_anchor_ids: anchors.iter().map(|anchor| anchor.id.clone()).collect(),
        required_anchor_kinds: ["constraint", "goal"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        required_terms: [
            "budget allocator",
            "contextpackbuilder",
            "replay",
            "src/core.rs",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        forbidden_terms: ["raw_tool_noise_sentinel"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        minimum_token_savings_ratio: 0.65,
        maximum_after_input_tokens: 2_048,
        minimum_score: 100.0,
        require_no_required_drop: true,
        require_atomic_tool_groups: true,
        require_typed_epoch: true,
        require_epoch_parent_chain: true,
        require_ledger_preserved: true,
        require_replay_pack_match: true,
        require_replay_anchor_match: true,
        require_restart_pack_match: true,
        require_restart_anchor_match: true,
    };
    let corpus = ContextCompactionEvalCorpus {
        schema_version: CONTEXT_COMPACTION_CORPUS_SCHEMA_VERSION.to_string(),
        cases: vec![rubric.clone()],
    };
    assert!(corpus.validate().is_ok());
    let baseline_observation = ContextCompactionEvalObservation {
        schema_version: openagent_eval::CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION.to_string(),
        case_id: rubric.case_id.clone(),
        before_input_tokens: 10_000,
        after_input_tokens: 1_800,
        after_budget_limit_tokens: Some(2_000),
        observed_anchor_ids: rubric.required_anchor_ids.clone(),
        observed_anchor_kinds: rubric.required_anchor_kinds.clone(),
        retained_terms: rubric.required_terms.clone(),
        forbidden_terms_present: BTreeSet::new(),
        required_drop_count: 0,
        allocation_overflowed: false,
        split_tool_group_count: 0,
        continuity: ContextCompactionContinuity {
            typed_epoch_count: 2,
            epoch_parent_chain_valid: true,
            ledger_preserved: true,
            replay_pack_hash_match: true,
            replay_anchor_registry_match: true,
            restart_pack_hash_match: true,
            restart_anchor_registry_match: true,
        },
    };
    let baseline = score_context_compaction(&rubric, &baseline_observation);
    let mut degraded_observation = baseline_observation.clone();
    degraded_observation.after_input_tokens = 7_000;
    degraded_observation
        .observed_anchor_kinds
        .remove("constraint");
    degraded_observation.retained_terms.remove("replay");
    degraded_observation
        .forbidden_terms_present
        .insert("raw_tool_noise_sentinel".to_string());
    degraded_observation.required_drop_count = 1;
    degraded_observation.allocation_overflowed = true;
    degraded_observation.split_tool_group_count = 1;
    degraded_observation.continuity = ContextCompactionContinuity::default();
    let degraded = score_context_compaction(&rubric, &degraded_observation);
    let comparison = compare_context_compaction(
        &baseline,
        &degraded,
        0.0,
        0.0,
        0,
        baseline_observation.after_input_tokens,
        degraded_observation.after_input_tokens,
    );
    json!({
        "corpus": corpus,
        "baseline_observation": baseline_observation,
        "baseline_result": baseline,
        "degraded_observation": degraded_observation,
        "degraded_result": degraded,
        "comparison": comparison,
    })
}

fn compaction_rubric(anchors: &[SemanticAnchor]) -> ContextCompactionEvalRubric {
    ContextCompactionEvalRubric {
        case_id: "builder-long-session".to_string(),
        description: "Typed epoch and anchors replace noisy historical turns".to_string(),
        required_anchor_ids: anchors.iter().map(|anchor| anchor.id.clone()).collect(),
        required_anchor_kinds: [
            "blocker",
            "constraint",
            "decision",
            "file",
            "goal",
            "next_step",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        required_terms: [
            "budget allocator",
            "contextpackbuilder",
            "provider boundary",
            "replay",
            "restart",
            "src/core.rs",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        forbidden_terms: ["raw_tool_noise_sentinel"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        minimum_token_savings_ratio: 0.65,
        maximum_after_input_tokens: 2_200,
        minimum_score: 100.0,
        require_no_required_drop: true,
        require_atomic_tool_groups: true,
        require_typed_epoch: true,
        require_epoch_parent_chain: true,
        require_ledger_preserved: true,
        require_replay_pack_match: true,
        require_replay_anchor_match: true,
        require_restart_pack_match: true,
        require_restart_anchor_match: true,
    }
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

fn read_fixture() -> Result<Value, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/golden/rust_rewrite/eval_integrations.json");
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

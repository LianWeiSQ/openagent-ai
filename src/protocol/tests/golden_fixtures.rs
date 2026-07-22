use std::{collections::BTreeMap, fs, path::PathBuf};

use openagent_protocol::{
    AgentDescriptor, AgentResult, AgentSpec, ArtifactRef, ChatMessage, ContextEpoch, FanoutBudget,
    FinishReason, Model, ModelCapabilities, ModelPricing, PermissionMode, PermissionRuleset,
    RUNTIME_OPTION_KEYS, Role, RunContext, RunLimits, RunStatus, SemanticAnchor,
    SemanticAnchorAuthority, SemanticAnchorKind, SemanticAnchorRegistry, SemanticAnchorScope,
    StreamEvent, SwarmUsage, ToolCall, ToolDefinitionSchemaFixture, ToolExecutionSchema,
    ToolExecutionScope, ToolResult, ToolSchema, Usage, WorkState, WorkStateFile,
    materialize_openai_compatible_payload, render_work_state, ruleset,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn core_protocol_fixture_matches_legacy_oracle() {
    assert_fixture_eq("core_protocol.json", core_protocol_fixture());
}

#[test]
fn permission_rulesets_fixture_matches_legacy_oracle() {
    let rulesets = [
        PermissionRuleset::Full,
        PermissionRuleset::None,
        PermissionRuleset::PlanOnly,
        PermissionRuleset::Readonly,
    ]
    .into_iter()
    .map(|name| (name.as_str().to_string(), ruleset(name)))
    .collect::<BTreeMap<_, _>>();

    assert_fixture_eq(
        "permission_rulesets.json",
        json!({
            "schema_version": 1,
            "rulesets": rulesets,
        }),
    );
}

#[test]
fn swarm_protocol_fixture_matches_legacy_oracle() {
    assert_fixture_eq("swarm_protocol.json", swarm_protocol_fixture());
}

#[test]
fn tool_definition_schema_fixture_matches_legacy_oracle() {
    assert_fixture_eq(
        "tool_definition_schema.json",
        tool_definition_schema_fixture(),
    );
}

#[test]
fn context_state_fixture_matches_legacy_oracle() {
    assert_fixture_eq("context_state.json", context_state_fixture());
}

#[test]
fn semantic_anchor_registry_fixture_is_versioned_and_deterministic() {
    let old_goal = SemanticAnchor::new(
        SemanticAnchorKind::Goal,
        "primary",
        "Old goal.",
        SemanticAnchorAuthority::StructuredWorkState,
        SemanticAnchorScope::Session,
        "context_epoch:old",
    );
    let goal = SemanticAnchor::new(
        SemanticAnchorKind::Goal,
        "primary",
        "Ship semantic anchors.",
        SemanticAnchorAuthority::Explicit,
        SemanticAnchorScope::Session,
        "api.context",
    )
    .with_references(vec!["goal:root".to_string()]);
    let decision = SemanticAnchor::new(
        SemanticAnchorKind::Decision,
        "registry-contract",
        "Use typed, versioned anchors.",
        SemanticAnchorAuthority::Explicit,
        SemanticAnchorScope::Session,
        "api.context",
    );
    let registry = SemanticAnchorRegistry::build(vec![old_goal, goal, decision]);
    assert_fixture_eq(
        "semantic_anchor_registry.json",
        json!({
            "schema_version": 1,
            "registry": registry,
            "diagnostics": registry.diagnostics(),
        }),
    );
}

fn core_protocol_fixture() -> Value {
    let model = fixture_model();
    let tool_schema = fixture_tool_schema();
    let tool_call = ToolCall {
        name: "read".to_string(),
        input: json!({"path": "README.md"}),
        call_id: "call_fixture_read".to_string(),
    };
    let mut result_metadata = BTreeMap::new();
    result_metadata.insert("bytes".to_string(), json!(24));
    result_metadata.insert("tool".to_string(), json!("read"));
    let tool_result = ToolResult {
        call_id: tool_call.call_id.clone(),
        output: "OpenAgent fixture output".to_string(),
        error: None,
        metadata: result_metadata,
    };
    let messages = fixture_messages(&tool_call, &tool_result);
    let options = BTreeMap::from([
        ("temperature".to_string(), json!(0.2)),
        ("trace".to_string(), json!({"enabled": true})),
        ("runtime_warnings".to_string(), json!({"enabled": true})),
    ]);
    let payload = materialize_openai_compatible_payload(
        Some("You are OpenAgent."),
        &messages,
        std::slice::from_ref(&tool_schema),
        Some(&model),
        Some(&options),
    );
    let runtime_option_keys = RUNTIME_OPTION_KEYS.to_vec();
    let mut step_tokens = BTreeMap::new();
    step_tokens.insert("input".to_string(), 123);
    step_tokens.insert("output".to_string(), 45);
    let stream_events = vec![
        StreamEvent::StepStart {
            snapshot_id: "snapshot_fixture".to_string(),
        },
        StreamEvent::TextStart {
            id: "text_fixture".to_string(),
            metadata: Some(json!({"channel": "final"})),
        },
        StreamEvent::TextDelta {
            id: "text_fixture".to_string(),
            text: "hello".to_string(),
        },
        StreamEvent::TextEnd {
            id: "text_fixture".to_string(),
        },
        StreamEvent::ToolCall {
            name: tool_call.name.clone(),
            input: tool_call.input.clone(),
            call_id: tool_call.call_id.clone(),
        },
        StreamEvent::ToolResult {
            call_id: tool_result.call_id.clone(),
            output: tool_result.output.clone(),
            error: None,
            metadata: Some(json!(tool_result.metadata)),
        },
        StreamEvent::StepFinish {
            tokens: step_tokens,
            cost: 0.00123,
            finish_reason: FinishReason::Stop,
        },
    ];

    json!({
        "schema_version": 1,
        "model": model,
        "tool_schema": tool_schema,
        "tool_call": tool_call,
        "tool_call_key": tool_call.key(),
        "tool_result": tool_result,
        "usage": Usage { input_tokens: 123, output_tokens: 45, cost: 0.00123 },
        "stream_events": stream_events,
        "runtime_option_keys": runtime_option_keys,
        "openai_payload": value(payload),
    })
}

fn swarm_protocol_fixture() -> Value {
    let spec = AgentSpec {
        role: "reviewer".to_string(),
        objective: "Review a deterministic fixture.".to_string(),
        context: "The fixture should be stable and network-free.".to_string(),
        boundaries: "Do not modify files.".to_string(),
        output_schema: json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "confidence": {"type": "number"},
            },
            "required": ["summary"],
        }),
        inputs: BTreeMap::from([("path".to_string(), json!("README.md"))]),
        limits: RunLimits {
            max_steps: Some(4),
            max_input_tokens: Some(2048),
            max_output_tokens: Some(512),
            max_cost: Some(0.25),
            timeout_seconds: Some(json!(30)),
        },
        permissions: PermissionMode::Readonly,
        metadata: BTreeMap::from([("fixture".to_string(), json!(true))]),
    };
    let result = AgentResult {
        status: RunStatus::Completed,
        summary: "Fixture review completed.".to_string(),
        evidence: vec!["README.md fixture evidence".to_string()],
        open_questions: Vec::new(),
        confidence: 0.92,
        artifacts: vec![ArtifactRef {
            kind: "trace".to_string(),
            uri: "runs/fixture/trace.jsonl".to_string(),
            title: "Trace".to_string(),
            metadata: BTreeMap::new(),
        }],
        usage: SwarmUsage {
            input_tokens: 10,
            output_tokens: 5,
            cost: 0.0001,
            steps: 1,
            latency_ms: 12,
        },
        metadata: BTreeMap::from([("runner".to_string(), json!("fixture"))]),
    };

    json!({
        "schema_version": 1,
        "budget": FanoutBudget {
            max_concurrent: 0,
            max_total_workers: 0,
            max_total_tokens: Some(100),
            max_total_cost: Some(1.5),
        }.normalized(),
        "descriptor": AgentDescriptor {
            id: "fixture-runner".to_string(),
            roles: vec!["reviewer".to_string(), "*".to_string()],
            tool_groups: vec!["readonly".to_string()],
            model_tier: "worker".to_string(),
            max_context: 8192,
            supports_streaming: true,
            kind: "function".to_string(),
            metadata: BTreeMap::from([("fixture".to_string(), json!(true))]),
        },
        "run_context": RunContext {
            run_id: "run_fixture".to_string(),
            parent_span_id: Some("span_parent".to_string()),
            budget: FanoutBudget::default(),
            cancellation: None,
            metadata: BTreeMap::from([("fixture".to_string(), json!(true))]),
        },
        "spec": spec,
        "result": result,
    })
}

fn tool_definition_schema_fixture() -> Value {
    json!(ToolDefinitionSchemaFixture {
        schema_version: 1,
        tool_id: "fixture_tool".to_string(),
        description: "Fixture tool for Rust schema parity.".to_string(),
        group: "fixture".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: ToolExecutionSchema::readonly("fixture", Some(2)),
        parameters_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path inside the workspace.",
                },
                "mode": {
                    "type": "string",
                    "enum": ["preview", "apply"],
                },
                "max_lines": {"type": "integer"},
                "include_hidden": {"type": "boolean"},
                "labels": {
                    "type": "array",
                    "items": {"type": "string"},
                },
                "weights": {
                    "type": "object",
                    "additionalProperties": {"type": "integer"},
                },
            },
            "required": ["path"],
        }),
    })
}

fn context_state_fixture() -> Value {
    let state = fixture_work_state();
    let rendered = render_work_state(&state);
    let mut context_epoch = ContextEpoch::manual(
        "epoch-fixture",
        "session-fixture",
        "run-fixture",
        1_781_840_000_000,
        7,
        Some("message-7".to_string()),
        rendered.clone(),
    )
    .into_automatic("model_context_budget", "sha1:before-pack", 3, 256, state);
    context_epoch.parent_epoch_id = Some("epoch-previous".to_string());
    context_epoch.boundary_message_id = Some("message-epoch-fixture".to_string());
    json!({
        "schema_version": 1,
        "rendered": rendered,
        "context_epoch": context_epoch,
    })
}

fn fixture_model() -> Model {
    Model {
        id: "gpt-fixture".to_string(),
        provider_id: "openai".to_string(),
        name: "Fixture Model".to_string(),
        context_window: 128000,
        max_output: 4096,
        capabilities: ModelCapabilities {
            vision: true,
            tools: true,
            streaming: true,
            reasoning: false,
        },
        pricing: ModelPricing {
            input_per_1m: 1.25,
            output_per_1m: 10.0,
        },
    }
}

fn fixture_tool_schema() -> ToolSchema {
    ToolSchema {
        name: "read".to_string(),
        description: "Read a workspace file.".to_string(),
        schema: Some(json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        })),
        group: "workspace".to_string(),
        dangerous: false,
    }
}

fn fixture_messages(tool_call: &ToolCall, tool_result: &ToolResult) -> Vec<ChatMessage> {
    let mut tool_call_metadata = BTreeMap::new();
    tool_call_metadata.insert(
        "tool_calls".to_string(),
        json!([
            {
                "id": tool_call.call_id,
                "type": "function",
                "function": {
                    "name": tool_call.name,
                    "arguments": "{\"path\": \"README.md\"}",
                },
            }
        ]),
    );
    vec![
        ChatMessage {
            role: Role::User,
            content: "Inspect README.md.".to_string(),
            name: None,
            tool_call_id: None,
            metadata: BTreeMap::new(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_call_id: None,
            metadata: tool_call_metadata,
        },
        ChatMessage {
            role: Role::Tool,
            content: tool_result.output.clone(),
            name: None,
            tool_call_id: Some(tool_result.call_id.clone()),
            metadata: BTreeMap::new(),
        },
        ChatMessage {
            role: Role::Assistant,
            content: "README.md was inspected.".to_string(),
            name: None,
            tool_call_id: None,
            metadata: BTreeMap::new(),
        },
    ]
}

fn fixture_work_state() -> WorkState {
    WorkState {
        task: "Freeze Python behavior for Rust rewrite.".to_string(),
        constraints: Vec::new(),
        progress: vec!["Captured protocol fixtures.".to_string()],
        decisions: vec!["Fixtures must be deterministic.".to_string()],
        files: vec![WorkStateFile {
            path: "doc/rust-rewrite-plan.md".to_string(),
            status: "created".to_string(),
            note: "Goal 0 contract.".to_string(),
        }],
        tool_findings: vec!["No live network calls are required.".to_string()],
        todos: vec!["Compare Rust serde output against fixtures.".to_string()],
        open_questions: Vec::new(),
        blockers: Vec::new(),
        next_steps: vec!["Implement Rust protocol crate.".to_string()],
        risks: vec!["Later live-provider smoke tests need credentials.".to_string()],
        critical_context: Vec::new(),
    }
}

fn assert_fixture_eq(name: &str, actual: Value) {
    let expected = read_fixture(name);
    assert_eq!(actual, expected, "fixture drifted: {name}");
}

fn read_fixture(name: &str) -> Value {
    let path = repo_root()
        .join("tests")
        .join("golden")
        .join("rust_rewrite")
        .join(name);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("crate lives under src/protocol")
        .to_path_buf()
}

fn value(input: impl Serialize) -> Value {
    serde_json::to_value(input).expect("value serializes")
}

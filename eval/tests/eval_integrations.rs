use std::{error::Error, fs, path::PathBuf};

use openagent_eval::{
    ExplorationQualityObservation, ExplorationQualityRubric, ExplorationToolCall,
    compare_exploration_quality, eval_integrations_fixture, harbor_normalized_model_name,
    harbor_timeout_seconds, score_exploration_quality, terminal_bench_extract_returncode,
    terminal_bench_failure_mode,
};
use serde_json::Value;

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

fn read_fixture() -> Result<Value, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/golden/rust_rewrite/eval_integrations.json");
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

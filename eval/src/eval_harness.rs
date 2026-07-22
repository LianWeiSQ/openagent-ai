//! Eval, replay, CI gate, and benchmark contracts for the Rust rewrite.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use openagent_core::{ContextBudgetAllocationPhase, ContextDelivery, ContextPack};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const FIXTURE_ROOT: &str = "/tmp/openagent-rust-rewrite-fixture-goal13";

pub const DEFAULT_MAX_STEPS: i64 = 80;
pub const DEFAULT_CONTEXT_WINDOW: i64 = 128_000;
pub const DEFAULT_MAX_OUTPUT: i64 = 4096;
pub const DEFAULT_WORKDIR: &str = "/app";

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub fn core_crate_name() -> &'static str {
    openagent_core::crate_name()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    pub case_id: String,
    pub status: String,
    pub score: f64,
    pub duration_ms: i64,
    pub steps: i64,
    pub tool_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
    pub error_kind: Option<String>,
    pub failure_reasons: Vec<String>,
    pub trace_path: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub ledger_path: Option<String>,
    pub session_state_path: Option<String>,
    pub trace_summary_path: Option<String>,
    pub trace_check_ok: bool,
    pub trace_check_errors: Vec<String>,
    pub trace_event_count: i64,
    pub model_calls: i64,
    pub mcp_calls: i64,
    pub skill_calls: i64,
    pub local_tool_calls: i64,
    pub artifact_count: i64,
    pub error_count: i64,
    pub runtime_warning_count: i64,
    pub runtime_warning_codes: Vec<String>,
    pub total_latency_ms: i64,
    pub langfuse_trace_id: Option<String>,
    pub langfuse_scores_sent: bool,
    pub langfuse_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalCiGateResult {
    pub ok: bool,
    pub status: String,
    pub reasons: Vec<String>,
    pub metrics: Value,
    pub report_path: Option<String>,
    pub regression_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvalCiGateOptions {
    pub min_success_rate: f64,
    pub max_runtime_warnings: Option<i64>,
    pub require_trace_check: bool,
    pub fail_on_budget_regressions: bool,
    pub fail_on_status_regressions: bool,
}

impl Default for EvalCiGateOptions {
    fn default() -> Self {
        Self {
            min_success_rate: 1.0,
            max_runtime_warnings: None,
            require_trace_check: true,
            fail_on_budget_regressions: true,
            fail_on_status_regressions: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    pub cwd: String,
    pub returncode: i64,
    pub stderr: String,
    pub stdout: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarborCommandRecord {
    pub command: String,
    pub cwd: String,
    pub timeout_sec: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HarborSuccessSpec<'a> {
    pub command: &'a str,
    pub cwd: Option<&'a str>,
    pub timeout_ms: i64,
    pub workspace_root: &'a str,
    pub returncode: i64,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub elapsed_ms: i64,
}

pub const EXPLORATION_QUALITY_SCHEMA_VERSION: &str = "openagent.eval.exploration_quality.v1";
pub const CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION: &str = "openagent.eval.context_compaction.v1";
pub const CONTEXT_COMPACTION_CORPUS_SCHEMA_VERSION: &str =
    "openagent.eval.context_compaction_corpus.v1";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExplorationQualityRubric {
    pub case_id: String,
    pub required_context_kinds: BTreeSet<String>,
    pub required_available_tools: BTreeSet<String>,
    pub required_files: BTreeSet<String>,
    pub required_tools_used: BTreeSet<String>,
    pub required_answer_terms: BTreeSet<String>,
    pub forbidden_tools: BTreeSet<String>,
    pub max_failed_tool_calls: u64,
    pub max_duplicate_tool_calls: u64,
    pub minimum_score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplorationToolCall {
    pub call_id: String,
    pub name: String,
    pub target: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExplorationQualityObservation {
    pub case_id: String,
    pub completed: bool,
    pub context_item_kinds: BTreeSet<String>,
    pub available_tools: BTreeSet<String>,
    pub explored_files: BTreeSet<String>,
    pub tool_calls: Vec<ExplorationToolCall>,
    pub final_answer: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplorationQualityBreakdown {
    pub context_coverage: f64,
    pub available_tool_coverage: f64,
    pub file_coverage: f64,
    pub required_tool_coverage: f64,
    pub answer_coverage: f64,
    pub tool_efficiency: f64,
    pub completion: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplorationQualityResult {
    pub schema_version: String,
    pub case_id: String,
    pub passed: bool,
    pub score: f64,
    pub breakdown: ExplorationQualityBreakdown,
    pub missing_context_kinds: Vec<String>,
    pub missing_available_tools: Vec<String>,
    pub missing_files: Vec<String>,
    pub missing_tools_used: Vec<String>,
    pub missing_answer_terms: Vec<String>,
    pub forbidden_tools_used: Vec<String>,
    pub failed_tool_calls: u64,
    pub duplicate_tool_calls: u64,
    pub failure_reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplorationQualityComparison {
    pub schema_version: String,
    pub case_id: String,
    pub passed: bool,
    pub baseline_score: f64,
    pub current_score: f64,
    pub score_delta: f64,
    pub regressions: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionEvalRubric {
    pub case_id: String,
    pub description: String,
    pub required_anchor_ids: BTreeSet<String>,
    pub required_anchor_kinds: BTreeSet<String>,
    pub required_terms: BTreeSet<String>,
    pub forbidden_terms: BTreeSet<String>,
    pub minimum_token_savings_ratio: f64,
    pub maximum_after_input_tokens: u64,
    pub minimum_score: f64,
    pub require_no_required_drop: bool,
    pub require_atomic_tool_groups: bool,
    pub require_typed_epoch: bool,
    pub require_epoch_parent_chain: bool,
    pub require_ledger_preserved: bool,
    pub require_replay_pack_match: bool,
    pub require_replay_anchor_match: bool,
    pub require_restart_pack_match: bool,
    pub require_restart_anchor_match: bool,
}

impl ContextCompactionEvalRubric {
    pub fn validate(&self) -> Result<(), String> {
        if self.case_id.trim().is_empty() {
            return Err("context compaction eval case_id must not be empty".to_string());
        }
        if !(0.0..=1.0).contains(&self.minimum_token_savings_ratio) {
            return Err(
                "context compaction minimum_token_savings_ratio must be between 0 and 1"
                    .to_string(),
            );
        }
        if self.maximum_after_input_tokens == 0 {
            return Err(
                "context compaction maximum_after_input_tokens must be positive".to_string(),
            );
        }
        if !(0.0..=100.0).contains(&self.minimum_score) {
            return Err("context compaction minimum_score must be between 0 and 100".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionEvalCorpus {
    pub schema_version: String,
    pub cases: Vec<ContextCompactionEvalRubric>,
}

impl ContextCompactionEvalCorpus {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONTEXT_COMPACTION_CORPUS_SCHEMA_VERSION {
            return Err(format!(
                "unsupported context compaction corpus schema: {}",
                self.schema_version
            ));
        }
        if self.cases.is_empty() {
            return Err("context compaction corpus must contain at least one case".to_string());
        }
        let mut ids = BTreeSet::new();
        for case in &self.cases {
            case.validate()?;
            if !ids.insert(case.case_id.clone()) {
                return Err(format!(
                    "duplicate context compaction eval case: {}",
                    case.case_id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionContinuity {
    pub typed_epoch_count: u64,
    pub epoch_parent_chain_valid: bool,
    pub ledger_preserved: bool,
    pub replay_pack_hash_match: bool,
    pub replay_anchor_registry_match: bool,
    pub restart_pack_hash_match: bool,
    pub restart_anchor_registry_match: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionEvalObservation {
    pub schema_version: String,
    pub case_id: String,
    pub before_input_tokens: u64,
    pub after_input_tokens: u64,
    pub after_budget_limit_tokens: Option<u64>,
    pub observed_anchor_ids: BTreeSet<String>,
    pub observed_anchor_kinds: BTreeSet<String>,
    pub retained_terms: BTreeSet<String>,
    pub forbidden_terms_present: BTreeSet<String>,
    pub required_drop_count: u64,
    pub allocation_overflowed: bool,
    pub split_tool_group_count: u64,
    pub continuity: ContextCompactionContinuity,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionEvalBreakdown {
    pub semantic_anchor_recall: f64,
    pub semantic_term_recall: f64,
    pub noise_removal: f64,
    pub token_reduction: f64,
    pub budget_fit: f64,
    pub provider_integrity: f64,
    pub recovery_consistency: f64,
    pub durable_state: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionEvalResult {
    pub schema_version: String,
    pub case_id: String,
    pub passed: bool,
    pub score: f64,
    pub token_savings: u64,
    pub token_savings_ratio: f64,
    pub breakdown: ContextCompactionEvalBreakdown,
    pub missing_anchor_ids: Vec<String>,
    pub missing_anchor_kinds: Vec<String>,
    pub missing_terms: Vec<String>,
    pub forbidden_terms_present: Vec<String>,
    pub required_drop_count: u64,
    pub allocation_overflowed: bool,
    pub split_tool_group_count: u64,
    pub failure_reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextCompactionEvalComparison {
    pub schema_version: String,
    pub case_id: String,
    pub passed: bool,
    pub baseline_score: f64,
    pub current_score: f64,
    pub score_delta: f64,
    pub baseline_token_savings_ratio: f64,
    pub current_token_savings_ratio: f64,
    pub after_input_token_delta: i64,
    pub regressions: Vec<String>,
}

#[must_use]
pub fn exploration_observation_from_bridge_turn(
    case_id: &str,
    turn: &Value,
    context: &Value,
) -> ExplorationQualityObservation {
    let receipt = context
        .get("latest")
        .and_then(|latest| latest.get("receipt"))
        .unwrap_or(&Value::Null);
    let context_item_kinds = receipt
        .get("item_kind_counts")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, count)| count.as_u64().unwrap_or_default() > 0)
        .map(|(kind, _)| kind.clone())
        .collect::<BTreeSet<_>>();
    let available_tools = receipt
        .get("tool_names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();

    let mut started = BTreeMap::<String, (String, Option<String>)>::new();
    let mut finished = BTreeMap::<String, String>::new();
    for event in turn
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let method = event.get("method").and_then(Value::as_str).unwrap_or("");
        let params = event.get("params").unwrap_or(&Value::Null);
        let call_id = params
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if call_id.is_empty() {
            continue;
        }
        if method == "item/toolCall/started" {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let target = params.get("input").and_then(exploration_tool_target);
            started.insert(call_id.to_string(), (name, target));
        } else if method == "item/toolCall/completed" {
            finished.insert(call_id.to_string(), "completed".to_string());
        } else if method == "item/toolCall/failed" {
            finished.insert(call_id.to_string(), "failed".to_string());
        }
    }

    let mut tool_calls = started
        .into_iter()
        .map(|(call_id, (name, target))| ExplorationToolCall {
            status: finished
                .get(&call_id)
                .cloned()
                .unwrap_or_else(|| "pending".to_string()),
            call_id,
            name,
            target,
        })
        .collect::<Vec<_>>();
    tool_calls.sort_by(|left, right| left.call_id.cmp(&right.call_id));
    let explored_files = tool_calls
        .iter()
        .filter(|call| call.name == "read" && call.status == "completed")
        .filter_map(|call| call.target.as_deref())
        .map(normalize_exploration_path)
        .collect::<BTreeSet<_>>();
    let final_answer = turn
        .get("turn")
        .and_then(|value| value.get("final_answer"))
        .and_then(Value::as_str)
        .or_else(|| turn.get("final_answer").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    ExplorationQualityObservation {
        case_id: case_id.to_string(),
        completed: turn.get("status").and_then(Value::as_str) == Some("completed"),
        context_item_kinds,
        available_tools,
        explored_files,
        tool_calls,
        final_answer,
    }
}

#[must_use]
pub fn score_exploration_quality(
    rubric: &ExplorationQualityRubric,
    observation: &ExplorationQualityObservation,
) -> ExplorationQualityResult {
    let observed_tools = observation
        .tool_calls
        .iter()
        .map(|call| call.name.clone())
        .collect::<BTreeSet<_>>();
    let missing_context_kinds = missing_values(
        &rubric.required_context_kinds,
        &observation.context_item_kinds,
    );
    let missing_available_tools = missing_values(
        &rubric.required_available_tools,
        &observation.available_tools,
    );
    let missing_files = missing_values(&rubric.required_files, &observation.explored_files);
    let missing_tools_used = missing_values(&rubric.required_tools_used, &observed_tools);
    let answer = observation.final_answer.to_ascii_lowercase();
    let missing_answer_terms = rubric
        .required_answer_terms
        .iter()
        .filter(|term| !answer.contains(&term.to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let forbidden_tools_used = rubric
        .forbidden_tools
        .intersection(&observed_tools)
        .cloned()
        .collect::<Vec<_>>();
    let failed_tool_calls = observation
        .tool_calls
        .iter()
        .filter(|call| call.status != "completed")
        .count() as u64;
    let unique_tool_calls = observation
        .tool_calls
        .iter()
        .map(|call| {
            format!(
                "{}:{}",
                call.name,
                call.target.as_deref().unwrap_or_default()
            )
        })
        .collect::<BTreeSet<_>>();
    let duplicate_tool_calls = observation
        .tool_calls
        .len()
        .saturating_sub(unique_tool_calls.len()) as u64;
    let tool_efficiency = if failed_tool_calls <= rubric.max_failed_tool_calls
        && duplicate_tool_calls <= rubric.max_duplicate_tool_calls
    {
        1.0
    } else {
        0.0
    };
    let breakdown = ExplorationQualityBreakdown {
        context_coverage: coverage(
            rubric.required_context_kinds.len(),
            missing_context_kinds.len(),
        ),
        available_tool_coverage: coverage(
            rubric.required_available_tools.len(),
            missing_available_tools.len(),
        ),
        file_coverage: coverage(rubric.required_files.len(), missing_files.len()),
        required_tool_coverage: coverage(
            rubric.required_tools_used.len(),
            missing_tools_used.len(),
        ),
        answer_coverage: coverage(
            rubric.required_answer_terms.len(),
            missing_answer_terms.len(),
        ),
        tool_efficiency,
        completion: if observation.completed { 1.0 } else { 0.0 },
    };
    let score = breakdown.context_coverage * 15.0
        + breakdown.available_tool_coverage * 10.0
        + breakdown.file_coverage * 25.0
        + breakdown.required_tool_coverage * 15.0
        + breakdown.answer_coverage * 25.0
        + breakdown.tool_efficiency * 5.0
        + breakdown.completion * 5.0;
    let mut failure_reasons = Vec::new();
    push_missing_reason(
        &mut failure_reasons,
        "context kinds",
        &missing_context_kinds,
    );
    push_missing_reason(
        &mut failure_reasons,
        "available tools",
        &missing_available_tools,
    );
    push_missing_reason(&mut failure_reasons, "files", &missing_files);
    push_missing_reason(&mut failure_reasons, "tools used", &missing_tools_used);
    push_missing_reason(&mut failure_reasons, "answer terms", &missing_answer_terms);
    if !forbidden_tools_used.is_empty() {
        failure_reasons.push(format!(
            "forbidden tools used: {}",
            forbidden_tools_used.join(", ")
        ));
    }
    if failed_tool_calls > rubric.max_failed_tool_calls {
        failure_reasons.push(format!(
            "failed tool calls exceeded limit: {failed_tool_calls} > {}",
            rubric.max_failed_tool_calls
        ));
    }
    if duplicate_tool_calls > rubric.max_duplicate_tool_calls {
        failure_reasons.push(format!(
            "duplicate tool calls exceeded limit: {duplicate_tool_calls} > {}",
            rubric.max_duplicate_tool_calls
        ));
    }
    if !observation.completed {
        failure_reasons.push("turn did not complete".to_string());
    }
    if score < rubric.minimum_score {
        failure_reasons.push(format!(
            "quality score below minimum: {score:.3} < {:.3}",
            rubric.minimum_score
        ));
    }
    let passed = failure_reasons.is_empty();

    ExplorationQualityResult {
        schema_version: EXPLORATION_QUALITY_SCHEMA_VERSION.to_string(),
        case_id: if rubric.case_id.is_empty() {
            observation.case_id.clone()
        } else {
            rubric.case_id.clone()
        },
        passed,
        score,
        breakdown,
        missing_context_kinds,
        missing_available_tools,
        missing_files,
        missing_tools_used,
        missing_answer_terms,
        forbidden_tools_used,
        failed_tool_calls,
        duplicate_tool_calls,
        failure_reasons,
    }
}

#[must_use]
pub fn compare_exploration_quality(
    baseline: &ExplorationQualityResult,
    current: &ExplorationQualityResult,
    maximum_score_regression: f64,
) -> ExplorationQualityComparison {
    let mut regressions = Vec::new();
    let score_delta = current.score - baseline.score;
    if score_delta < -maximum_score_regression.abs() {
        regressions.push(format!(
            "score regressed by {:.3}, allowed regression is {:.3}",
            -score_delta,
            maximum_score_regression.abs()
        ));
    }
    for (name, baseline_value, current_value) in [
        (
            "context_coverage",
            baseline.breakdown.context_coverage,
            current.breakdown.context_coverage,
        ),
        (
            "available_tool_coverage",
            baseline.breakdown.available_tool_coverage,
            current.breakdown.available_tool_coverage,
        ),
        (
            "file_coverage",
            baseline.breakdown.file_coverage,
            current.breakdown.file_coverage,
        ),
        (
            "required_tool_coverage",
            baseline.breakdown.required_tool_coverage,
            current.breakdown.required_tool_coverage,
        ),
        (
            "answer_coverage",
            baseline.breakdown.answer_coverage,
            current.breakdown.answer_coverage,
        ),
    ] {
        if current_value < baseline_value {
            regressions.push(format!(
                "{name} regressed: {current_value:.3} < {baseline_value:.3}"
            ));
        }
    }
    if current.failed_tool_calls > baseline.failed_tool_calls {
        regressions.push(format!(
            "failed tool calls increased: {} > {}",
            current.failed_tool_calls, baseline.failed_tool_calls
        ));
    }
    if current.duplicate_tool_calls > baseline.duplicate_tool_calls {
        regressions.push(format!(
            "duplicate tool calls increased: {} > {}",
            current.duplicate_tool_calls, baseline.duplicate_tool_calls
        ));
    }
    if baseline.passed && !current.passed {
        regressions.push("current quality gate failed after a passing baseline".to_string());
    }
    ExplorationQualityComparison {
        schema_version: EXPLORATION_QUALITY_SCHEMA_VERSION.to_string(),
        case_id: current.case_id.clone(),
        passed: regressions.is_empty(),
        baseline_score: baseline.score,
        current_score: current.score,
        score_delta,
        regressions,
    }
}

#[must_use]
pub fn context_compaction_observation_from_packs(
    case_id: &str,
    before: &ContextPack,
    after: &ContextPack,
    required_terms: &BTreeSet<String>,
    forbidden_terms: &BTreeSet<String>,
    continuity: ContextCompactionContinuity,
) -> ContextCompactionEvalObservation {
    let provider_text = after
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    let observed_anchor_ids = after
        .semantic_anchor_registry
        .anchors
        .iter()
        .map(|anchor| anchor.id.clone())
        .collect();
    let observed_anchor_kinds = after
        .semantic_anchor_registry
        .anchors
        .iter()
        .map(|anchor| anchor.kind.as_str().to_string())
        .collect();
    let retained_terms = observed_terms(required_terms, &provider_text);
    let forbidden_terms_present = observed_terms(forbidden_terms, &provider_text);
    let required_drop_count = after
        .trace
        .iter()
        .filter(|entry| {
            !entry.included && entry.drop_reason.as_deref() == Some("required_budget_exhausted")
        })
        .count() as u64;
    let allocation_overflowed = after.budget.overflowed
        || after
            .budget
            .allocation
            .as_ref()
            .is_some_and(|allocation| allocation.hard_overflow_tokens > 0);
    let split_tool_group_count = split_tool_group_count(after);

    ContextCompactionEvalObservation {
        schema_version: CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION.to_string(),
        case_id: case_id.to_string(),
        before_input_tokens: before.estimated_input_tokens,
        after_input_tokens: after.estimated_input_tokens,
        after_budget_limit_tokens: after.budget.input_limit_tokens,
        observed_anchor_ids,
        observed_anchor_kinds,
        retained_terms,
        forbidden_terms_present,
        required_drop_count,
        allocation_overflowed,
        split_tool_group_count,
        continuity,
    }
}

#[must_use]
pub fn context_compaction_observation_from_bridge_context(
    case_id: &str,
    before_receipt: &Value,
    after_context: &Value,
    provider_projection: &str,
    required_terms: &BTreeSet<String>,
    forbidden_terms: &BTreeSet<String>,
    continuity: ContextCompactionContinuity,
) -> ContextCompactionEvalObservation {
    let latest = after_context.get("latest").unwrap_or(after_context);
    let receipt = latest.get("receipt").unwrap_or(&Value::Null);
    let trace = latest
        .get("trace")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let provider_text = provider_projection.to_ascii_lowercase();
    let observed_anchor_ids = trace
        .iter()
        .filter_map(|entry| entry.pointer("/semantic_anchor/id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let observed_anchor_kinds = receipt
        .pointer("/semantic_anchor_registry/kind_counts")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, count)| count.as_u64().unwrap_or_default() > 0)
        .map(|(kind, _)| kind.clone())
        .collect::<BTreeSet<_>>();
    let required_drop_count = receipt
        .pointer("/drop_reason_counts/required_budget_exhausted")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let allocation_overflowed = receipt
        .pointer("/budget/overflowed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || receipt
            .pointer("/budget/allocation/hard_overflow_tokens")
            .and_then(Value::as_u64)
            .is_some_and(|tokens| tokens > 0);
    let split_tool_group_count = split_public_tool_group_count(&trace);

    ContextCompactionEvalObservation {
        schema_version: CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION.to_string(),
        case_id: case_id.to_string(),
        before_input_tokens: receipt_source_input_tokens(before_receipt),
        after_input_tokens: receipt
            .get("estimated_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        after_budget_limit_tokens: receipt
            .pointer("/budget/input_limit_tokens")
            .and_then(Value::as_u64),
        observed_anchor_ids,
        observed_anchor_kinds,
        retained_terms: observed_terms(required_terms, &provider_text),
        forbidden_terms_present: observed_terms(forbidden_terms, &provider_text),
        required_drop_count,
        allocation_overflowed,
        split_tool_group_count,
        continuity,
    }
}

#[must_use]
pub fn score_context_compaction(
    rubric: &ContextCompactionEvalRubric,
    observation: &ContextCompactionEvalObservation,
) -> ContextCompactionEvalResult {
    let missing_anchor_ids = missing_values(
        &rubric.required_anchor_ids,
        &observation.observed_anchor_ids,
    );
    let missing_anchor_kinds = missing_values(
        &rubric.required_anchor_kinds,
        &observation.observed_anchor_kinds,
    );
    let missing_terms = missing_values(&rubric.required_terms, &observation.retained_terms);
    let forbidden_terms_present = rubric
        .forbidden_terms
        .intersection(&observation.forbidden_terms_present)
        .cloned()
        .collect::<Vec<_>>();
    let token_savings = observation
        .before_input_tokens
        .saturating_sub(observation.after_input_tokens);
    let token_savings_ratio = if observation.before_input_tokens == 0 {
        0.0
    } else {
        token_savings as f64 / observation.before_input_tokens as f64
    };
    let expected_anchor_count = rubric
        .required_anchor_ids
        .len()
        .saturating_add(rubric.required_anchor_kinds.len());
    let missing_anchor_count = missing_anchor_ids
        .len()
        .saturating_add(missing_anchor_kinds.len());
    let after_within_observed_budget = observation
        .after_budget_limit_tokens
        .is_none_or(|limit| observation.after_input_tokens <= limit);
    let budget_fit = observation.after_input_tokens <= rubric.maximum_after_input_tokens
        && after_within_observed_budget
        && !observation.allocation_overflowed;
    let provider_integrity = (!rubric.require_no_required_drop
        || observation.required_drop_count == 0)
        && (!rubric.require_atomic_tool_groups || observation.split_tool_group_count == 0);
    let recovery_checks = [
        (
            rubric.require_replay_pack_match,
            observation.continuity.replay_pack_hash_match,
        ),
        (
            rubric.require_replay_anchor_match,
            observation.continuity.replay_anchor_registry_match,
        ),
        (
            rubric.require_restart_pack_match,
            observation.continuity.restart_pack_hash_match,
        ),
        (
            rubric.require_restart_anchor_match,
            observation.continuity.restart_anchor_registry_match,
        ),
    ];
    let durable_checks = [
        (
            rubric.require_typed_epoch,
            observation.continuity.typed_epoch_count > 0,
        ),
        (
            rubric.require_epoch_parent_chain,
            observation.continuity.epoch_parent_chain_valid,
        ),
        (
            rubric.require_ledger_preserved,
            observation.continuity.ledger_preserved,
        ),
    ];
    let breakdown = ContextCompactionEvalBreakdown {
        semantic_anchor_recall: coverage(expected_anchor_count, missing_anchor_count),
        semantic_term_recall: coverage(rubric.required_terms.len(), missing_terms.len()),
        noise_removal: coverage(rubric.forbidden_terms.len(), forbidden_terms_present.len()),
        token_reduction: threshold_progress(
            token_savings_ratio,
            rubric.minimum_token_savings_ratio,
        ),
        budget_fit: f64::from(budget_fit),
        provider_integrity: f64::from(provider_integrity),
        recovery_consistency: required_check_coverage(&recovery_checks),
        durable_state: required_check_coverage(&durable_checks),
    };
    let score = breakdown.semantic_anchor_recall * 25.0
        + breakdown.semantic_term_recall * 20.0
        + breakdown.noise_removal * 10.0
        + breakdown.token_reduction * 15.0
        + breakdown.budget_fit * 10.0
        + breakdown.provider_integrity * 5.0
        + breakdown.recovery_consistency * 10.0
        + breakdown.durable_state * 5.0;
    let mut failure_reasons = Vec::new();
    if let Err(error) = rubric.validate() {
        failure_reasons.push(error);
    }
    if observation.schema_version != CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION {
        failure_reasons.push(format!(
            "unsupported context compaction observation schema: {}",
            observation.schema_version
        ));
    }
    if !observation.case_id.trim().is_empty()
        && !rubric.case_id.trim().is_empty()
        && observation.case_id != rubric.case_id
    {
        failure_reasons.push(format!(
            "context compaction case mismatch: observation={} rubric={}",
            observation.case_id, rubric.case_id
        ));
    }
    push_missing_reason(&mut failure_reasons, "anchor IDs", &missing_anchor_ids);
    push_missing_reason(&mut failure_reasons, "anchor kinds", &missing_anchor_kinds);
    push_missing_reason(&mut failure_reasons, "semantic terms", &missing_terms);
    if !forbidden_terms_present.is_empty() {
        failure_reasons.push(format!(
            "forbidden terms remained: {}",
            forbidden_terms_present.join(", ")
        ));
    }
    if token_savings_ratio < rubric.minimum_token_savings_ratio {
        failure_reasons.push(format!(
            "token savings below minimum: {token_savings_ratio:.3} < {:.3}",
            rubric.minimum_token_savings_ratio
        ));
    }
    if observation.after_input_tokens > rubric.maximum_after_input_tokens {
        failure_reasons.push(format!(
            "post-compaction tokens exceeded limit: {} > {}",
            observation.after_input_tokens, rubric.maximum_after_input_tokens
        ));
    }
    if !after_within_observed_budget {
        failure_reasons.push("post-compaction input exceeded its model budget".to_string());
    }
    if observation.allocation_overflowed {
        failure_reasons.push("post-compaction allocation still overflowed".to_string());
    }
    if rubric.require_no_required_drop && observation.required_drop_count > 0 {
        failure_reasons.push(format!(
            "required context was dropped: {} item(s)",
            observation.required_drop_count
        ));
    }
    if rubric.require_atomic_tool_groups && observation.split_tool_group_count > 0 {
        failure_reasons.push(format!(
            "tool exchange groups were split: {} group(s)",
            observation.split_tool_group_count
        ));
    }
    push_required_check_failures(
        &mut failure_reasons,
        &recovery_checks,
        &[
            "replay pack hash did not match",
            "replay anchor registry did not match",
            "restart pack hash did not match",
            "restart anchor registry did not match",
        ],
    );
    push_required_check_failures(
        &mut failure_reasons,
        &durable_checks,
        &[
            "typed context epoch was not observed",
            "context epoch parent chain was invalid",
            "source session ledger was not preserved",
        ],
    );
    if score < rubric.minimum_score {
        failure_reasons.push(format!(
            "compaction quality score below minimum: {score:.3} < {:.3}",
            rubric.minimum_score
        ));
    }

    ContextCompactionEvalResult {
        schema_version: CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION.to_string(),
        case_id: if rubric.case_id.is_empty() {
            observation.case_id.clone()
        } else {
            rubric.case_id.clone()
        },
        passed: failure_reasons.is_empty(),
        score,
        token_savings,
        token_savings_ratio,
        breakdown,
        missing_anchor_ids,
        missing_anchor_kinds,
        missing_terms,
        forbidden_terms_present,
        required_drop_count: observation.required_drop_count,
        allocation_overflowed: observation.allocation_overflowed,
        split_tool_group_count: observation.split_tool_group_count,
        failure_reasons,
    }
}

#[must_use]
pub fn compare_context_compaction(
    baseline: &ContextCompactionEvalResult,
    current: &ContextCompactionEvalResult,
    maximum_score_regression: f64,
    maximum_savings_ratio_regression: f64,
    maximum_after_token_increase: u64,
    baseline_after_input_tokens: u64,
    current_after_input_tokens: u64,
) -> ContextCompactionEvalComparison {
    let mut regressions = Vec::new();
    if baseline.schema_version != CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION
        || current.schema_version != CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION
    {
        regressions.push("unsupported context compaction result schema".to_string());
    }
    if baseline.case_id != current.case_id {
        regressions.push(format!(
            "context compaction comparison case mismatch: {} != {}",
            baseline.case_id, current.case_id
        ));
    }
    let score_delta = current.score - baseline.score;
    if score_delta < -maximum_score_regression.abs() {
        regressions.push(format!(
            "score regressed by {:.3}, allowed regression is {:.3}",
            -score_delta,
            maximum_score_regression.abs()
        ));
    }
    let savings_delta = current.token_savings_ratio - baseline.token_savings_ratio;
    if savings_delta < -maximum_savings_ratio_regression.abs() {
        regressions.push(format!(
            "token savings ratio regressed by {:.3}, allowed regression is {:.3}",
            -savings_delta,
            maximum_savings_ratio_regression.abs()
        ));
    }
    let after_input_token_delta =
        current_after_input_tokens as i128 - baseline_after_input_tokens as i128;
    if after_input_token_delta > i128::from(maximum_after_token_increase) {
        regressions.push(format!(
            "post-compaction tokens increased by {after_input_token_delta}, allowed increase is {maximum_after_token_increase}"
        ));
    }
    for (name, baseline_value, current_value) in [
        (
            "semantic_anchor_recall",
            baseline.breakdown.semantic_anchor_recall,
            current.breakdown.semantic_anchor_recall,
        ),
        (
            "semantic_term_recall",
            baseline.breakdown.semantic_term_recall,
            current.breakdown.semantic_term_recall,
        ),
        (
            "noise_removal",
            baseline.breakdown.noise_removal,
            current.breakdown.noise_removal,
        ),
        (
            "provider_integrity",
            baseline.breakdown.provider_integrity,
            current.breakdown.provider_integrity,
        ),
        (
            "recovery_consistency",
            baseline.breakdown.recovery_consistency,
            current.breakdown.recovery_consistency,
        ),
        (
            "durable_state",
            baseline.breakdown.durable_state,
            current.breakdown.durable_state,
        ),
    ] {
        if current_value < baseline_value {
            regressions.push(format!(
                "{name} regressed: {current_value:.3} < {baseline_value:.3}"
            ));
        }
    }
    if baseline.passed && !current.passed {
        regressions.push("current compaction gate failed after a passing baseline".to_string());
    }
    ContextCompactionEvalComparison {
        schema_version: CONTEXT_COMPACTION_EVAL_SCHEMA_VERSION.to_string(),
        case_id: current.case_id.clone(),
        passed: regressions.is_empty(),
        baseline_score: baseline.score,
        current_score: current.score,
        score_delta,
        baseline_token_savings_ratio: baseline.token_savings_ratio,
        current_token_savings_ratio: current.token_savings_ratio,
        after_input_token_delta: after_input_token_delta
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX))
            as i64,
        regressions,
    }
}

#[must_use]
pub fn aggregate_results(results: &[EvalResult]) -> Value {
    let total = results.len() as i64;
    let passed = results
        .iter()
        .filter(|result| result.status == "pass")
        .count() as i64;
    json!({
        "total_cases": total,
        "passed": passed,
        "failed": total - passed,
        "success_rate": if total == 0 { 0.0 } else { passed as f64 / total as f64 },
        "average_steps": average(results.iter().map(|result| result.steps as f64)),
        "average_model_calls": average(results.iter().map(|result| result.model_calls as f64)),
        "average_tool_calls": average(results.iter().map(|result| result.tool_calls as f64)),
        "total_input_tokens": results.iter().map(|result| result.input_tokens).sum::<i64>(),
        "total_output_tokens": results.iter().map(|result| result.output_tokens).sum::<i64>(),
        "total_cost": results.iter().map(|result| result.cost).sum::<f64>(),
        "average_duration_ms": average(results.iter().map(|result| result.duration_ms as f64)),
        "average_total_latency_ms": average(results.iter().map(|result| result.total_latency_ms as f64)),
        "trace_check_passed": results.iter().filter(|result| result.trace_check_ok).count() as i64,
        "trace_check_failed": results.iter().filter(|result| !result.trace_check_ok).count() as i64,
        "mcp_calls": results.iter().map(|result| result.mcp_calls).sum::<i64>(),
        "skill_calls": results.iter().map(|result| result.skill_calls).sum::<i64>(),
        "local_tool_calls": results.iter().map(|result| result.local_tool_calls).sum::<i64>(),
        "artifact_count": results.iter().map(|result| result.artifact_count).sum::<i64>(),
        "error_count": results.iter().map(|result| result.error_count).sum::<i64>(),
        "runtime_warning_count": results.iter().map(|result| result.runtime_warning_count).sum::<i64>(),
    })
}

#[must_use]
pub fn render_summary(results: &[EvalResult]) -> String {
    let aggregate = aggregate_results(results);
    let total = int_value(&aggregate["total_cases"]);
    let passed = int_value(&aggregate["passed"]);
    let mut lines = vec![
        "# OpenAgent Eval Summary".to_string(),
        String::new(),
        format!("- Total cases: {total}"),
        format!("- Passed: {passed}"),
        format!("- Failed: {}", total - passed),
        format!(
            "- Success rate: {:.1}%",
            if total == 0 {
                0.0
            } else {
                passed as f64 / total as f64 * 100.0
            }
        ),
        format!(
            "- Average steps: {:.2}",
            float_value(&aggregate["average_steps"])
        ),
        format!(
            "- Average model calls: {:.2}",
            float_value(&aggregate["average_model_calls"])
        ),
        format!(
            "- Average tool calls: {:.2}",
            float_value(&aggregate["average_tool_calls"])
        ),
        format!(
            "- Total input tokens: {}",
            int_value(&aggregate["total_input_tokens"])
        ),
        format!(
            "- Total output tokens: {}",
            int_value(&aggregate["total_output_tokens"])
        ),
        format!("- Total cost: {:.6}", float_value(&aggregate["total_cost"])),
        format!(
            "- Average duration: {:.2} ms",
            float_value(&aggregate["average_duration_ms"])
        ),
        format!(
            "- Trace checks passed: {}",
            int_value(&aggregate["trace_check_passed"])
        ),
        format!(
            "- Trace checks failed: {}",
            int_value(&aggregate["trace_check_failed"])
        ),
        format!(
            "- Runtime warnings: {}",
            int_value(&aggregate["runtime_warning_count"])
        ),
        format!(
            "- Tool sources: local={} skill={} mcp={}",
            int_value(&aggregate["local_tool_calls"]),
            int_value(&aggregate["skill_calls"]),
            int_value(&aggregate["mcp_calls"])
        ),
    ];

    if let Some(slowest) = results.iter().max_by_key(|result| result.duration_ms) {
        lines.push(format!(
            "- Slowest case: {} ({} ms)",
            slowest.case_id, slowest.duration_ms
        ));
    }
    if let Some(priciest) = results
        .iter()
        .max_by(|left, right| compare_f64(left.cost, right.cost))
    {
        lines.push(format!(
            "- Most expensive case: {} ({:.6})",
            priciest.case_id, priciest.cost
        ));
    }

    let failures = results
        .iter()
        .flat_map(|result| result.failure_reasons.iter().cloned())
        .collect::<BTreeSet<_>>();
    if !failures.is_empty() {
        lines.push(String::new());
        lines.push("## Failure Reasons".to_string());
        lines.extend(failures.into_iter().map(|reason| format!("- {reason}")));
    }
    lines.join("\n") + "\n"
}

#[must_use]
pub fn compare_with_baseline(
    baseline_report_path: &str,
    baseline_report: &Value,
    current_results: &[EvalResult],
    current_report_path: &str,
    thresholds: Option<&Value>,
) -> Value {
    let baseline_results = baseline_report
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let raw_thresholds = thresholds.or_else(|| baseline_report.get("regression_thresholds"));
    let threshold_config = normalize_regression_thresholds(raw_thresholds);

    let baseline_by_id = baseline_results
        .iter()
        .filter_map(|item| {
            let case_id = item.get("case_id")?.as_str()?;
            Some((case_id.to_string(), item.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let current_by_id = current_results
        .iter()
        .filter_map(|result| {
            serde_json::to_value(result)
                .ok()
                .map(|value| (result.case_id.clone(), value))
        })
        .collect::<BTreeMap<_, _>>();

    let mut case_ids = baseline_by_id.keys().cloned().collect::<BTreeSet<_>>();
    case_ids.extend(current_by_id.keys().cloned());
    let mut cases = Vec::new();
    for case_id in case_ids {
        let baseline = baseline_by_id.get(&case_id);
        let current = current_by_id.get(&case_id);
        match (baseline, current) {
            (None, Some(current_value)) => cases.push(json!({
                "case_id": case_id,
                "change": "new_case",
                "current": case_regression_fields(current_value),
            })),
            (Some(baseline_value), None) => cases.push(json!({
                "case_id": case_id,
                "change": "removed_case",
                "baseline": case_regression_fields(baseline_value),
            })),
            (Some(baseline_value), Some(current_value)) => {
                let score_delta =
                    float_field(current_value, "score") - float_field(baseline_value, "score");
                let cost_delta =
                    float_field(current_value, "cost") - float_field(baseline_value, "cost");
                let duration_delta_ms = int_field(current_value, "duration_ms")
                    - int_field(baseline_value, "duration_ms");
                let input_tokens_delta = int_field(current_value, "input_tokens")
                    - int_field(baseline_value, "input_tokens");
                let output_tokens_delta = int_field(current_value, "output_tokens")
                    - int_field(baseline_value, "output_tokens");
                let total_tokens_delta = (int_field(current_value, "input_tokens")
                    + int_field(current_value, "output_tokens"))
                    - (int_field(baseline_value, "input_tokens")
                        + int_field(baseline_value, "output_tokens"));
                let tool_calls_delta = int_field(current_value, "tool_calls")
                    - int_field(baseline_value, "tool_calls");
                let model_calls_delta = int_field(current_value, "model_calls")
                    - int_field(baseline_value, "model_calls");
                let baseline_status = string_field(baseline_value, "status");
                let current_status = string_field(current_value, "status");
                let budget_regressions = budget_regressions(
                    &[
                        ("cost_delta", cost_delta),
                        ("duration_delta_ms", duration_delta_ms as f64),
                        ("input_tokens_delta", input_tokens_delta as f64),
                        ("output_tokens_delta", output_tokens_delta as f64),
                        ("total_tokens_delta", total_tokens_delta as f64),
                        ("tool_calls_delta", tool_calls_delta as f64),
                        ("model_calls_delta", model_calls_delta as f64),
                    ],
                    &threshold_config,
                );
                cases.push(json!({
                    "case_id": case_id,
                    "change": "compared",
                    "baseline": case_regression_fields(baseline_value),
                    "current": case_regression_fields(current_value),
                    "status_changed": baseline_status != current_status,
                    "status_regression": baseline_status == "pass" && current_status != "pass",
                    "status_improvement": baseline_status != "pass" && current_status == "pass",
                    "score_delta": score_delta,
                    "cost_delta": cost_delta,
                    "duration_delta_ms": duration_delta_ms,
                    "input_tokens_delta": input_tokens_delta,
                    "output_tokens_delta": output_tokens_delta,
                    "total_tokens_delta": total_tokens_delta,
                    "tool_calls_delta": tool_calls_delta,
                    "model_calls_delta": model_calls_delta,
                    "budget_regressions": budget_regressions,
                }));
            }
            (None, None) => {}
        }
    }

    let threshold_value = threshold_config
        .iter()
        .map(|(key, value)| (key.clone(), json!(value)))
        .collect::<Map<_, _>>();
    let summary = json!({
        "baseline_report": baseline_report_path,
        "current_report": current_report_path,
        "regression_thresholds": threshold_value,
        "baseline_total": baseline_by_id.len() as i64,
        "current_total": current_by_id.len() as i64,
        "new_cases": cases.iter().filter(|item| item["change"] == "new_case").count() as i64,
        "removed_cases": cases.iter().filter(|item| item["change"] == "removed_case").count() as i64,
        "status_regressions": cases.iter().filter(|item| bool_field(item, "status_regression")).count() as i64,
        "status_improvements": cases.iter().filter(|item| bool_field(item, "status_improvement")).count() as i64,
        "score_regressions": cases.iter().filter(|item| float_field(item, "score_delta") < 0.0).count() as i64,
        "cost_increased_cases": cases.iter().filter(|item| float_field(item, "cost_delta") > 0.0).count() as i64,
        "input_tokens_increased_cases": cases.iter().filter(|item| int_field(item, "input_tokens_delta") > 0).count() as i64,
        "output_tokens_increased_cases": cases.iter().filter(|item| int_field(item, "output_tokens_delta") > 0).count() as i64,
        "total_tokens_increased_cases": cases.iter().filter(|item| int_field(item, "total_tokens_delta") > 0).count() as i64,
        "duration_increased_cases": cases.iter().filter(|item| int_field(item, "duration_delta_ms") > 0).count() as i64,
        "budget_regressions": cases.iter().filter(|item| {
            item.get("budget_regressions")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
        }).count() as i64,
    });

    json!({"summary": summary, "cases": cases})
}

#[must_use]
pub fn render_regression_summary(regression: &Value) -> String {
    let summary = regression.get("summary").and_then(Value::as_object);
    let summary_value = regression.get("summary").unwrap_or(&Value::Null);
    let mut lines = vec![
        "# OpenAgent Eval Regression".to_string(),
        String::new(),
        format!(
            "- Baseline cases: {}",
            int_field(summary_value, "baseline_total")
        ),
        format!(
            "- Current cases: {}",
            int_field(summary_value, "current_total")
        ),
        format!("- New cases: {}", int_field(summary_value, "new_cases")),
        format!(
            "- Removed cases: {}",
            int_field(summary_value, "removed_cases")
        ),
        format!(
            "- Status regressions: {}",
            int_field(summary_value, "status_regressions")
        ),
        format!(
            "- Status improvements: {}",
            int_field(summary_value, "status_improvements")
        ),
        format!(
            "- Score regressions: {}",
            int_field(summary_value, "score_regressions")
        ),
        format!(
            "- Cost increased cases: {}",
            int_field(summary_value, "cost_increased_cases")
        ),
        format!(
            "- Token increased cases: {}",
            int_field(summary_value, "total_tokens_increased_cases")
        ),
        format!(
            "- Duration increased cases: {}",
            int_field(summary_value, "duration_increased_cases")
        ),
        format!(
            "- Budget regressions: {}",
            int_field(summary_value, "budget_regressions")
        ),
    ];

    if let Some(thresholds) = summary
        .and_then(|item| item.get("regression_thresholds"))
        .and_then(Value::as_object)
        .filter(|item| !item.is_empty())
    {
        lines.push(String::new());
        lines.push("## Regression Thresholds".to_string());
        let mut keys = thresholds.keys().collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            lines.push(format!(
                "- {key}: {}",
                format_g(float_value(&thresholds[key]))
            ));
        }
    }

    let interesting = regression
        .get("cases")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    string_field(item, "change") != "compared"
                        || bool_field(item, "status_changed")
                        || float_field(item, "score_delta") != 0.0
                        || float_field(item, "cost_delta") != 0.0
                        || int_field(item, "total_tokens_delta") != 0
                        || int_field(item, "duration_delta_ms") != 0
                        || item
                            .get("budget_regressions")
                            .and_then(Value::as_array)
                            .is_some_and(|items| !items.is_empty())
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !interesting.is_empty() {
        lines.push(String::new());
        lines.push("## Case Changes".to_string());
        for item in interesting {
            let case_id = string_field(&item, "case_id");
            let change = string_field(&item, "change");
            if change != "compared" {
                lines.push(format!("- {case_id}: {change}"));
                continue;
            }
            let baseline_status = item
                .get("baseline")
                .map(|value| string_field(value, "status"))
                .unwrap_or_default();
            let current_status = item
                .get("current")
                .map(|value| string_field(value, "status"))
                .unwrap_or_default();
            lines.push(format!(
                "- {case_id}: {baseline_status} -> {current_status}, score_delta={:.3}, cost_delta={:.6}, tokens_delta={}, duration_delta_ms={}",
                float_field(&item, "score_delta"),
                float_field(&item, "cost_delta"),
                int_field(&item, "total_tokens_delta"),
                int_field(&item, "duration_delta_ms"),
            ));
            if let Some(reasons) = item.get("budget_regressions").and_then(Value::as_array) {
                for reason in reasons.iter().filter_map(Value::as_str) {
                    lines.push(format!("  - budget: {reason}"));
                }
            }
        }
    }
    lines.join("\n") + "\n"
}

#[must_use]
pub fn check_eval_ci_gate(
    report_path: &str,
    report: &Value,
    regression: Option<(&str, &Value)>,
    options: EvalCiGateOptions,
) -> EvalCiGateResult {
    let aggregate = report.get("aggregate").unwrap_or(&Value::Null);
    let results = report
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let success_rate = aggregate
        .get("success_rate")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| computed_success_rate(&results));
    let trace_check_failed = aggregate
        .get("trace_check_failed")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| count_trace_check_failed(&results));
    let runtime_warning_count = aggregate
        .get("runtime_warning_count")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| sum_result_int(&results, "runtime_warning_count"));
    let regression_summary = regression
        .map(|(_, value)| value)
        .or_else(|| report.get("regression"))
        .and_then(|value| value.get("summary"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut reasons = Vec::new();
    if success_rate < options.min_success_rate {
        reasons.push(format!(
            "success_rate below min_success_rate: {success_rate:.3} < {:.3}",
            options.min_success_rate
        ));
    }
    if options.require_trace_check && trace_check_failed > 0 {
        reasons.push(format!(
            "trace_check_failed must be 0: {trace_check_failed}"
        ));
    }
    if let Some(maximum) = options.max_runtime_warnings {
        if runtime_warning_count > maximum {
            reasons.push(format!(
                "runtime_warning_count exceeded max_runtime_warnings: {runtime_warning_count} > {maximum}"
            ));
        }
    }
    if options.fail_on_status_regressions
        && int_field(&regression_summary, "status_regressions") > 0
    {
        reasons.push(format!(
            "status_regressions must be 0: {}",
            int_field(&regression_summary, "status_regressions")
        ));
    }
    if options.fail_on_budget_regressions
        && int_field(&regression_summary, "budget_regressions") > 0
    {
        reasons.push(format!(
            "budget_regressions must be 0: {}",
            int_field(&regression_summary, "budget_regressions")
        ));
    }

    EvalCiGateResult {
        ok: reasons.is_empty(),
        status: if reasons.is_empty() { "pass" } else { "fail" }.to_string(),
        reasons,
        metrics: json!({
            "success_rate": success_rate,
            "min_success_rate": options.min_success_rate,
            "trace_check_failed": trace_check_failed,
            "runtime_warning_count": runtime_warning_count,
            "max_runtime_warnings": options.max_runtime_warnings,
            "status_regressions": int_field(&regression_summary, "status_regressions"),
            "budget_regressions": int_field(&regression_summary, "budget_regressions"),
        }),
        report_path: Some(report_path.to_string()),
        regression_path: regression.map(|(path, _)| path.to_string()),
    }
}

#[must_use]
pub fn langfuse_score_payloads(
    result: &EvalResult,
    case_id: &str,
    run_id: &str,
    trace_id: &str,
) -> Vec<Value> {
    let status_comment = if result.failure_reasons.is_empty() {
        format!("OpenAgent eval status for case {case_id}.")
    } else {
        result
            .failure_reasons
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    };
    vec![
        json!({
            "trace_id": trace_id,
            "score_id": format!("openagent:{run_id}:{case_id}:score"),
            "name": "openagent.eval.score",
            "value": result.score,
            "data_type": "NUMERIC",
            "comment": format!("OpenAgent eval score for case {case_id}."),
        }),
        json!({
            "trace_id": trace_id,
            "score_id": format!("openagent:{run_id}:{case_id}:status"),
            "name": "openagent.eval.status",
            "value": result.status,
            "data_type": "CATEGORICAL",
            "comment": status_comment,
        }),
        json!({
            "trace_id": trace_id,
            "score_id": format!("openagent:{run_id}:{case_id}:trace_check"),
            "name": "openagent.trace_check",
            "value": result.trace_check_ok,
            "data_type": "BOOLEAN",
            "comment": "OpenAgent trace integrity check result.",
        }),
        json!({
            "trace_id": trace_id,
            "score_id": format!("openagent:{run_id}:{case_id}:runtime_warning_count"),
            "name": "openagent.runtime_warning_count",
            "value": result.runtime_warning_count,
            "data_type": "NUMERIC",
            "comment": "OpenAgent runtime warning count for this eval case.",
        }),
    ]
}

#[must_use]
pub fn execution_metadata(mode: &str, workspace_root: &str, harness: &str) -> Value {
    json!({
        "execution_mode": mode,
        "workspace_root": workspace_root,
        "harness": harness,
    })
}

#[must_use]
pub fn display_path(workspace_root: &str, path: &str) -> String {
    let root = workspace_root.trim_end_matches('/');
    if path == root {
        return ".".to_string();
    }
    let prefix = format!("{root}/");
    path.strip_prefix(&prefix)
        .map_or_else(|| path.to_string(), ToString::to_string)
}

#[must_use]
pub fn terminal_bench_wrap_command(command: &str, cwd: Option<&str>, marker: &str) -> String {
    let mut lines = vec!["set +e".to_string()];
    if let Some(cwd) = cwd {
        lines.push(format!("cd {}", shell_quote(cwd)));
    }
    lines.extend([
        "(".to_string(),
        command.to_string(),
        ")".to_string(),
        "status=$?".to_string(),
        format!(
            "printf {} \"$status\"",
            shell_quote(&format!("\\n{marker}%s\\n"))
        ),
    ]);
    format!("bash -lc {}", shell_quote(&lines.join("\n")))
}

#[must_use]
pub fn terminal_bench_extract_returncode(observation: &str, marker: &str) -> (i64, String) {
    let pattern = format!(r"{}(?P<code>-?\d+)", regex::escape(marker));
    let Ok(regex) = Regex::new(&pattern) else {
        return (0, observation.to_string());
    };
    let code = regex
        .captures_iter(observation)
        .filter_map(|captures| captures.name("code"))
        .filter_map(|code| code.as_str().parse::<i64>().ok())
        .last();
    match code {
        Some(returncode) => (
            returncode,
            regex.replace_all(observation, "").trim().to_string(),
        ),
        None => (0, observation.to_string()),
    }
}

#[must_use]
pub fn terminal_bench_format_observation(
    observation: &str,
    returncode: i64,
    elapsed_ms: i64,
) -> String {
    let body = observation.trim();
    let suffix =
        format!("[openagent terminal-bench] exit_code={returncode} duration_ms={elapsed_ms}");
    if body.is_empty() {
        suffix
    } else {
        format!("{body}\n{suffix}")
    }
}

#[must_use]
pub fn terminal_bench_failure_mode(message: &str) -> &'static str {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("timeout") {
        "agent_timeout"
    } else if lowered.contains("context") && lowered.contains("length") {
        "context_length_exceeded"
    } else if lowered.contains("output") && lowered.contains("length") {
        "output_length_exceeded"
    } else {
        "unknown_agent_error"
    }
}

#[must_use]
pub fn terminal_bench_system_prompt(workspace_root: &str) -> String {
    format!(
        "You are OpenAgent running inside Terminal-Bench. Complete the task by using only the bash tool.\n\
The bash tool executes commands in the benchmark tmux session. The default workspace is {workspace_root}.\n\
Directory changes do not persist between tool calls, so use absolute paths or combine commands with `cd <dir> && ...`.\n\
Inspect the environment, modify files with shell commands when needed, run validation commands, and iterate from failures.\n\
Do not ask the user questions. When the task is complete, provide a concise final answer."
    )
}

#[must_use]
pub fn harbor_timeout_seconds(timeout_ms: i64) -> i64 {
    let seconds = (timeout_ms as f64 / 1000.0).ceil() as i64;
    seconds.max(1)
}

#[must_use]
pub fn harbor_success_command(spec: HarborSuccessSpec<'_>) -> (HarborCommandRecord, CommandResult) {
    let resolved_cwd = spec.cwd.unwrap_or(spec.workspace_root).to_string();
    let command_record = HarborCommandRecord {
        command: spec.command.to_string(),
        cwd: resolved_cwd.clone(),
        timeout_sec: harbor_timeout_seconds(spec.timeout_ms),
    };
    let suffix = format!(
        "[openagent harbor] exit_code={} duration_ms={}",
        spec.returncode, spec.elapsed_ms
    );
    let formatted_stdout = if spec.stdout.trim().is_empty() {
        suffix
    } else {
        format!("{}\n{suffix}", spec.stdout.trim_end())
    };
    (
        command_record,
        CommandResult {
            cwd: resolved_cwd,
            returncode: spec.returncode,
            stderr: spec.stderr.to_string(),
            stdout: formatted_stdout,
        },
    )
}

#[must_use]
pub fn harbor_timeout_command(
    command: &str,
    cwd: Option<&str>,
    timeout_ms: i64,
    workspace_root: &str,
    elapsed_ms: i64,
    error: &str,
) -> (HarborCommandRecord, CommandResult) {
    let resolved_cwd = cwd.unwrap_or(workspace_root).to_string();
    let command_record = HarborCommandRecord {
        command: command.to_string(),
        cwd: resolved_cwd.clone(),
        timeout_sec: harbor_timeout_seconds(timeout_ms),
    };
    (
        command_record,
        CommandResult {
            cwd: resolved_cwd,
            returncode: 124,
            stderr: error.to_string(),
            stdout: format!("[openagent harbor] exit_code=124 duration_ms={elapsed_ms}"),
        },
    )
}

#[must_use]
pub fn harbor_normalized_model_name(value: Option<&str>) -> Option<String> {
    let raw = value.unwrap_or("").trim();
    if raw.is_empty() {
        return None;
    }
    let Some((provider, model_name)) = raw.split_once('/') else {
        return Some(raw.to_string());
    };
    if matches!(
        provider.to_ascii_lowercase().as_str(),
        "openai" | "openai-compatible"
    ) {
        Some(model_name.to_string())
    } else {
        Some(raw.to_string())
    }
}

#[must_use]
pub fn harbor_system_prompt(workspace_root: &str) -> String {
    format!(
        "You are OpenAgent running inside Terminal-Bench 2.0 through Harbor. Complete the task by using only the bash tool.\n\
The bash tool executes commands in the benchmark environment. The default workspace is {workspace_root}.\n\
Each tool call can pass an explicit workdir; otherwise it runs in the default workspace.\n\
Inspect the environment, modify files with shell commands when needed, run validation commands, and iterate from failures.\n\
Do not ask the user questions. When the task is complete, provide a concise final answer."
    )
}

#[must_use]
pub fn eval_integrations_fixture() -> Value {
    let results = fixture_results();
    let aggregate = aggregate_results(&results);
    let summary = render_summary(&results);
    let baseline_report = baseline_report_fixture();
    let baseline_path = fixture_path("baseline.json");
    let current_report_path = fixture_path("report.json");
    let regression = compare_with_baseline(
        &baseline_path,
        &baseline_report,
        &results,
        &current_report_path,
        None,
    );
    let regression_summary = render_regression_summary(&regression);
    let report_payload = json!({
        "schema_version": "openagent.eval.report.v1",
        "aggregate": aggregate,
        "results": results,
        "regression": regression,
    });
    let clean_results = vec![fixture_pass_result()];
    let clean_report = json!({
        "schema_version": "openagent.eval.report.v1",
        "aggregate": aggregate_results(&clean_results),
        "results": clean_results,
    });
    let regression_path = fixture_path("regression.json");
    let ci_gate = json!({
        "pass": check_eval_ci_gate(
            &fixture_path("clean-report.json"),
            &clean_report,
            None,
            EvalCiGateOptions {
                max_runtime_warnings: Some(0),
                ..EvalCiGateOptions::default()
            },
        ),
        "fail": check_eval_ci_gate(
            &current_report_path,
            &report_payload,
            Some((&regression_path, &regression)),
            EvalCiGateOptions {
                min_success_rate: 0.75,
                max_runtime_warnings: Some(1),
                ..EvalCiGateOptions::default()
            },
        ),
    });

    let mut langfuse_success = fixture_fail_result();
    langfuse_success.langfuse_trace_id = Some("trace_fixture_123".to_string());
    langfuse_success.langfuse_scores_sent = true;
    let mut langfuse_failure = fixture_pass_result();
    langfuse_failure.langfuse_trace_id = Some("trace_fixture_123".to_string());
    langfuse_failure.langfuse_scores_sent = false;
    langfuse_failure.langfuse_error = Some("fixture score export failed".to_string());

    let (harbor_success_command, harbor_success_result) =
        harbor_success_command(HarborSuccessSpec {
            command: "echo hello",
            cwd: Some("/app/project"),
            timeout_ms: 5200,
            workspace_root: "/app",
            returncode: 7,
            stdout: "hello from harbor\n",
            stderr: "warn",
            elapsed_ms: 320,
        });
    let (harbor_timeout_command, harbor_timeout_result) =
        harbor_timeout_command("sleep 10", None, 1, "/app", 1234, "fixture timeout");
    let (terminal_code, terminal_cleaned) = terminal_bench_extract_returncode(
        "$ command\nhello\n__OPENAGENT_TBENCH_EXIT_fixture__7\ntrailing",
        "__OPENAGENT_TBENCH_EXIT_fixture__",
    );

    json!({
        "schema_version": 1,
        "eval": {
            "results": fixture_results(),
            "aggregate": aggregate,
            "summary": summary,
            "regression": regression,
            "regression_summary": regression_summary,
            "ci_gate": ci_gate,
            "langfuse": {
                "success_result": langfuse_success,
                "success_scores": langfuse_score_payloads(
                    &fixture_fail_result(),
                    "langfuse_case",
                    "run_fixture",
                    "trace_fixture_123",
                ),
                "success_flush_count": 1,
                "failure_result": langfuse_failure,
            },
        },
        "terminal_bench": {
            "defaults": {
                "max_steps": DEFAULT_MAX_STEPS,
                "context_window": DEFAULT_CONTEXT_WINDOW,
                "max_output": DEFAULT_MAX_OUTPUT,
                "workdir": DEFAULT_WORKDIR,
            },
            "metadata": execution_metadata("terminal_bench", "/app", "terminal_bench"),
            "display_paths": {
                "root": display_path("/app", "/app"),
                "nested": display_path("/app", "/app/project/file.txt"),
                "external": display_path("/app", "/tmp/file.txt"),
            },
            "wrapped_command": terminal_bench_wrap_command(
                "printf 'hello world'",
                Some("/app/project"),
                "__OPENAGENT_TBENCH_EXIT_fixture__",
            ),
            "extract_returncode": [json!(terminal_code), json!(terminal_cleaned)],
            "format_observation": {
                "with_body": terminal_bench_format_observation("hello", 7, 321),
                "empty": terminal_bench_format_observation("", 0, 1),
            },
            "failure_modes": {
                "timeout": terminal_bench_failure_mode("agent timeout"),
                "context": terminal_bench_failure_mode("context length exceeded"),
                "output": terminal_bench_failure_mode("output length exceeded"),
                "unknown": terminal_bench_failure_mode("boom"),
            },
            "system_prompt": terminal_bench_system_prompt("/workspace"),
        },
        "harbor": {
            "defaults": {
                "max_steps": DEFAULT_MAX_STEPS,
                "context_window": DEFAULT_CONTEXT_WINDOW,
                "max_output": DEFAULT_MAX_OUTPUT,
                "workdir": DEFAULT_WORKDIR,
            },
            "metadata": execution_metadata("harbor", "/app", "harbor"),
            "display_paths": {
                "root": display_path("/app", "/app"),
                "nested": display_path("/app", "/app/project/file.txt"),
                "external": display_path("/app", "/tmp/file.txt"),
            },
            "success_command": harbor_success_command,
            "success_result": harbor_success_result,
            "timeout_command": harbor_timeout_command,
            "timeout_result": harbor_timeout_result,
            "normalized_models": {
                "openai": harbor_normalized_model_name(Some("OpenAI/gpt-test")),
                "openai_compatible": harbor_normalized_model_name(Some("openai-compatible/gpt-test")),
                "other_provider": harbor_normalized_model_name(Some("vendor/model")),
                "plain": harbor_normalized_model_name(Some("plain-model")),
                "empty": harbor_normalized_model_name(Some("")),
            },
            "system_prompt": harbor_system_prompt("/workspace"),
        },
    })
}

fn fixture_results() -> Vec<EvalResult> {
    vec![fixture_pass_result(), fixture_fail_result()]
}

fn fixture_pass_result() -> EvalResult {
    EvalResult {
        case_id: "pass_case".to_string(),
        status: "pass".to_string(),
        score: 1.0,
        duration_ms: 1200,
        steps: 2,
        tool_calls: 1,
        input_tokens: 100,
        output_tokens: 40,
        cost: 0.0123,
        error_kind: None,
        failure_reasons: Vec::new(),
        trace_path: Some(fixture_path("pass.trace.jsonl")),
        session_id: Some("session_pass".to_string()),
        run_id: Some("run_fixture".to_string()),
        ledger_path: Some(fixture_path("pass.ledger.jsonl")),
        session_state_path: Some(fixture_path("pass.session.json")),
        trace_summary_path: Some(fixture_path("pass.summary.json")),
        trace_check_ok: true,
        trace_check_errors: Vec::new(),
        trace_event_count: 12,
        model_calls: 1,
        mcp_calls: 0,
        skill_calls: 1,
        local_tool_calls: 1,
        artifact_count: 1,
        error_count: 0,
        runtime_warning_count: 0,
        runtime_warning_codes: Vec::new(),
        total_latency_ms: 900,
        langfuse_trace_id: None,
        langfuse_scores_sent: false,
        langfuse_error: None,
    }
}

fn fixture_fail_result() -> EvalResult {
    EvalResult {
        case_id: "fail_case".to_string(),
        status: "fail".to_string(),
        score: 0.0,
        duration_ms: 4200,
        steps: 6,
        tool_calls: 4,
        input_tokens: 250,
        output_tokens: 60,
        cost: 0.09,
        error_kind: Some("model_error".to_string()),
        failure_reasons: vec![
            "final answer missing required text: expected".to_string(),
            "runtime warning count exceeded max_runtime_warnings: 2 > 0".to_string(),
        ],
        trace_path: Some(fixture_path("fail.trace.jsonl")),
        session_id: Some("session_fail".to_string()),
        run_id: Some("run_fixture".to_string()),
        ledger_path: Some(fixture_path("fail.ledger.jsonl")),
        session_state_path: Some(fixture_path("fail.session.json")),
        trace_summary_path: Some(fixture_path("fail.summary.json")),
        trace_check_ok: false,
        trace_check_errors: vec!["missing span".to_string()],
        trace_event_count: 20,
        model_calls: 3,
        mcp_calls: 1,
        skill_calls: 0,
        local_tool_calls: 3,
        artifact_count: 0,
        error_count: 1,
        runtime_warning_count: 2,
        runtime_warning_codes: vec![
            "step_total_tokens_exceeded".to_string(),
            "tool_call_failed".to_string(),
        ],
        total_latency_ms: 4100,
        langfuse_trace_id: None,
        langfuse_scores_sent: false,
        langfuse_error: None,
    }
}

fn baseline_report_fixture() -> Value {
    json!({
        "schema_version": "openagent.eval.report.v1",
        "regression_thresholds": {
            "max_cost_delta": 0.02,
            "max_duration_delta_ms": 500,
            "max_model_calls_delta": 1,
            "max_total_tokens_delta": 10,
        },
        "results": [
            {
                "case_id": "pass_case",
                "status": "pass",
                "score": 1.0,
                "duration_ms": 1000,
                "steps": 2,
                "model_calls": 1,
                "tool_calls": 1,
                "input_tokens": 90,
                "output_tokens": 30,
                "cost": 0.01,
                "trace_check_ok": true,
                "runtime_warning_count": 0,
            },
            {
                "case_id": "fail_case",
                "status": "pass",
                "score": 1.0,
                "duration_ms": 3000,
                "steps": 5,
                "model_calls": 1,
                "tool_calls": 2,
                "input_tokens": 200,
                "output_tokens": 50,
                "cost": 0.05,
                "trace_check_ok": true,
                "runtime_warning_count": 0,
            },
            {
                "case_id": "removed_case",
                "status": "pass",
                "score": 1.0,
                "duration_ms": 500,
                "steps": 1,
                "model_calls": 1,
                "tool_calls": 0,
                "input_tokens": 10,
                "output_tokens": 5,
                "cost": 0.001,
                "trace_check_ok": true,
                "runtime_warning_count": 0,
            },
        ],
    })
}

fn normalize_regression_thresholds(raw: Option<&Value>) -> BTreeMap<String, f64> {
    let allowed = [
        "max_cost_delta",
        "max_duration_delta_ms",
        "max_input_tokens_delta",
        "max_output_tokens_delta",
        "max_total_tokens_delta",
        "max_tool_calls_delta",
        "max_model_calls_delta",
    ];
    let Some(object) = raw.and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    allowed
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(Value::as_f64)
                .map(|value| (key.to_string(), value))
        })
        .collect()
}

fn budget_regressions(deltas: &[(&str, f64)], thresholds: &BTreeMap<String, f64>) -> Vec<String> {
    let checks = [
        ("max_cost_delta", "cost_delta"),
        ("max_duration_delta_ms", "duration_delta_ms"),
        ("max_input_tokens_delta", "input_tokens_delta"),
        ("max_output_tokens_delta", "output_tokens_delta"),
        ("max_total_tokens_delta", "total_tokens_delta"),
        ("max_tool_calls_delta", "tool_calls_delta"),
        ("max_model_calls_delta", "model_calls_delta"),
    ];
    checks
        .into_iter()
        .filter_map(|(threshold_key, delta_key)| {
            let threshold = thresholds.get(threshold_key)?;
            let delta = deltas
                .iter()
                .find_map(|(key, value)| (*key == delta_key).then_some(*value))
                .unwrap_or(0.0);
            (delta > *threshold).then(|| {
                format!(
                    "{delta_key} exceeded {threshold_key}: {} > {}",
                    format_g(delta),
                    format_g(*threshold)
                )
            })
        })
        .collect()
}

fn case_regression_fields(item: &Value) -> Value {
    let fields = [
        "status",
        "score",
        "duration_ms",
        "steps",
        "model_calls",
        "tool_calls",
        "input_tokens",
        "output_tokens",
        "cost",
        "trace_check_ok",
        "runtime_warning_count",
    ];
    let mut result = Map::new();
    for field in fields {
        if let Some(value) = item.get(field) {
            result.insert(field.to_string(), value.clone());
        }
    }
    Value::Object(result)
}

fn average(values: impl Iterator<Item = f64>) -> f64 {
    let materialized = values.collect::<Vec<_>>();
    if materialized.is_empty() {
        0.0
    } else {
        materialized.iter().sum::<f64>() / materialized.len() as f64
    }
}

fn compare_f64(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

fn exploration_tool_target(input: &Value) -> Option<String> {
    ["file_path", "path", "pattern", "query", "command"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(normalize_exploration_path)
        .filter(|value| !value.is_empty())
}

fn normalize_exploration_path(path: &str) -> String {
    let normalized = path.trim().replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .trim_end_matches('/')
        .to_string()
}

fn observed_terms(expected: &BTreeSet<String>, text: &str) -> BTreeSet<String> {
    expected
        .iter()
        .filter(|term| text.contains(&term.to_ascii_lowercase()))
        .cloned()
        .collect()
}

fn receipt_source_input_tokens(receipt: &Value) -> u64 {
    let selected = receipt
        .get("estimated_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let candidate_items = receipt
        .pointer("/budget/allocation/classes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|class| class.get("candidate_tokens").and_then(Value::as_u64))
        .sum::<u64>();
    let fixed_overhead = receipt
        .pointer("/budget/fixed_overhead_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    selected.max(candidate_items.saturating_add(fixed_overhead))
}

fn split_tool_group_count(pack: &ContextPack) -> u64 {
    let mut groups = BTreeMap::<String, BTreeSet<bool>>::new();
    for entry in pack
        .trace
        .iter()
        .filter(|entry| entry.delivery == ContextDelivery::Message)
    {
        let Some(decision) = entry.budget_allocation.as_ref().filter(|decision| {
            decision.is_current() && decision.phase != ContextBudgetAllocationPhase::Duplicate
        }) else {
            continue;
        };
        groups
            .entry(decision.group_id.clone())
            .or_default()
            .insert(entry.included);
    }
    groups.values().filter(|states| states.len() > 1).count() as u64
}

fn split_public_tool_group_count(trace: &[Value]) -> u64 {
    let mut groups = BTreeMap::<String, BTreeSet<bool>>::new();
    for entry in trace {
        let Some(group_id) = entry
            .pointer("/budget_allocation/group_id")
            .and_then(Value::as_str)
        else {
            continue;
        };
        if entry
            .pointer("/budget_allocation/phase")
            .and_then(Value::as_str)
            == Some("duplicate")
        {
            continue;
        }
        groups.entry(group_id.to_string()).or_default().insert(
            entry
                .get("included")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
    }
    groups.values().filter(|states| states.len() > 1).count() as u64
}

fn threshold_progress(actual: f64, required: f64) -> f64 {
    if required <= 0.0 {
        1.0
    } else {
        (actual / required).clamp(0.0, 1.0)
    }
}

fn required_check_coverage(checks: &[(bool, bool)]) -> f64 {
    let required = checks.iter().filter(|(required, _)| *required).count();
    if required == 0 {
        return 1.0;
    }
    let passed = checks
        .iter()
        .filter(|(required, passed)| *required && *passed)
        .count();
    passed as f64 / required as f64
}

fn push_required_check_failures(
    reasons: &mut Vec<String>,
    checks: &[(bool, bool)],
    messages: &[&str],
) {
    for ((required, passed), message) in checks.iter().zip(messages) {
        if *required && !*passed {
            reasons.push((*message).to_string());
        }
    }
}

fn missing_values(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> Vec<String> {
    expected.difference(actual).cloned().collect()
}

fn coverage(expected: usize, missing: usize) -> f64 {
    if expected == 0 {
        1.0
    } else {
        expected.saturating_sub(missing) as f64 / expected as f64
    }
}

fn push_missing_reason(reasons: &mut Vec<String>, label: &str, missing: &[String]) {
    if !missing.is_empty() {
        reasons.push(format!("missing {label}: {}", missing.join(", ")));
    }
}

fn computed_success_rate(results: &[Value]) -> f64 {
    if results.is_empty() {
        return 0.0;
    }
    let passed = results
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("pass"))
        .count();
    passed as f64 / results.len() as f64
}

fn count_trace_check_failed(results: &[Value]) -> i64 {
    results
        .iter()
        .filter(|item| {
            !item
                .get("trace_check_ok")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count() as i64
}

fn sum_result_int(results: &[Value], key: &str) -> i64 {
    results.iter().map(|item| int_field(item, key)).sum()
}

fn int_value(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|item| i64::try_from(item).ok()))
        .or_else(|| value.as_f64().map(|item| item as i64))
        .unwrap_or(0)
}

fn float_value(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn int_field(item: &Value, field: &str) -> i64 {
    item.get(field).map_or(0, int_value)
}

fn float_field(item: &Value, field: &str) -> f64 {
    item.get(field).map_or(0.0, float_value)
}

fn string_field(item: &Value, field: &str) -> String {
    item.get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn bool_field(item: &Value, field: &str) -> bool {
    item.get(field).and_then(Value::as_bool).unwrap_or(false)
}

fn format_g(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        return format!("{}", value.round() as i64);
    }
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_@%+=:,./-".contains(ch))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn fixture_path(name: &str) -> String {
    format!("{FIXTURE_ROOT}/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_to_core_crate() {
        assert_eq!(crate_name(), "openagent-eval");
        assert_eq!(core_crate_name(), "openagent-core");
    }

    #[test]
    fn terminal_command_quoting_matches_legacy_shape() {
        assert_eq!(
            terminal_bench_wrap_command(
                "printf 'hello world'",
                Some("/app/project"),
                "__OPENAGENT_TBENCH_EXIT_fixture__",
            ),
            "bash -lc 'set +e\ncd /app/project\n(\nprintf '\"'\"'hello world'\"'\"'\n)\nstatus=$?\nprintf '\"'\"'\\n__OPENAGENT_TBENCH_EXIT_fixture__%s\\n'\"'\"' \"$status\"'"
        );
    }
}

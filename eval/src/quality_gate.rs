use std::collections::{BTreeMap, BTreeSet};

use openagent_telemetry::{VersionIdentity, canonical_json_fingerprint};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const QUALITY_GATE_POLICY_SCHEMA_VERSION: &str = "openharness.quality_gate.policy.v1";
pub const QUALITY_GATE_EVIDENCE_SCHEMA_VERSION: &str = "openharness.quality_gate.evidence.v1";
pub const QUALITY_GATE_DECISION_SCHEMA_VERSION: &str = "openharness.quality_gate.decision.v1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGatePolicyV1 {
    pub schema_version: String,
    pub policy_id: String,
    pub min_success_rate: f64,
    pub max_degraded_rate: f64,
    pub min_trace_completeness: f64,
    pub max_privacy_violations: u64,
    pub max_status_regressions: u64,
    pub max_budget_regressions: u64,
    pub max_p95_duration_regression_ratio: f64,
    pub max_total_token_regression_ratio: f64,
    pub max_total_cost_regression_ratio: f64,
    pub require_case_links: bool,
    pub require_provenance: bool,
    pub required_critical_cases: BTreeSet<String>,
}

impl Default for QualityGatePolicyV1 {
    fn default() -> Self {
        Self {
            schema_version: QUALITY_GATE_POLICY_SCHEMA_VERSION.to_string(),
            policy_id: "openharness-default-release-v1".to_string(),
            min_success_rate: 0.95,
            max_degraded_rate: 0.05,
            min_trace_completeness: 0.95,
            max_privacy_violations: 0,
            max_status_regressions: 0,
            max_budget_regressions: 0,
            max_p95_duration_regression_ratio: 0.20,
            max_total_token_regression_ratio: 0.15,
            max_total_cost_regression_ratio: 0.15,
            require_case_links: true,
            require_provenance: true,
            required_critical_cases: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityCaseStatus {
    Pass,
    Degraded,
    Fail,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateSubjectV1 {
    pub candidate_id: String,
    pub versions: VersionIdentity,
    pub eval_dataset_version: String,
    pub eval_dataset_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityCaseEvidenceV1 {
    pub case_id: String,
    pub status: QualityCaseStatus,
    pub score: f64,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_microunits: u64,
    pub privacy_violations: u64,
    pub trace_completeness: f64,
    pub run_id: Option<String>,
    pub trace_id: Option<String>,
    pub task_contract_fingerprint: Option<String>,
    #[serde(default)]
    pub failure_reasons: Vec<String>,
}

impl QualityCaseEvidenceV1 {
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateBaselineV1 {
    pub baseline_id: String,
    pub case_count: u64,
    pub p95_duration_ms: u64,
    pub total_tokens: u64,
    pub total_cost_microunits: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityEvidenceProvenanceV1 {
    pub assembler_version: String,
    pub eval_report_fingerprint: String,
    pub baseline_report_fingerprint: String,
    pub regression_report_fingerprint: String,
    pub privacy_report_fingerprint: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityRegressionEvidenceV1 {
    pub status_regressions: u64,
    pub budget_regressions: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateEvidenceV1 {
    pub schema_version: String,
    pub evidence_id: String,
    pub generated_at_ms: u64,
    pub subject: QualityGateSubjectV1,
    pub provenance: QualityEvidenceProvenanceV1,
    pub cases: Vec<QualityCaseEvidenceV1>,
    pub baseline: QualityGateBaselineV1,
    #[serde(default)]
    pub regression: QualityRegressionEvidenceV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateMetricsV1 {
    pub total_cases: u64,
    pub passed_cases: u64,
    pub degraded_cases: u64,
    pub failed_cases: u64,
    pub success_rate: f64,
    pub degraded_rate: f64,
    pub trace_completeness: f64,
    pub privacy_violations: u64,
    pub p95_duration_ms: u64,
    pub total_tokens: u64,
    pub total_cost_microunits: u64,
    pub p95_duration_regression_ratio: Option<f64>,
    pub total_token_regression_ratio: Option<f64>,
    pub total_cost_regression_ratio: Option<f64>,
    pub status_regressions: u64,
    pub budget_regressions: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityGateDecisionStatus {
    Pass,
    Fail,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateDecisionV1 {
    pub schema_version: String,
    pub decision: QualityGateDecisionStatus,
    pub policy_id: String,
    pub evidence_id: String,
    pub evidence_fingerprint: String,
    pub reasons: Vec<String>,
    pub failing_cases: Vec<String>,
    pub metrics: QualityGateMetricsV1,
    pub evidence_links: BTreeMap<String, String>,
}

#[must_use]
pub fn evaluate_quality_gate(
    policy: &QualityGatePolicyV1,
    evidence: &QualityGateEvidenceV1,
) -> QualityGateDecisionV1 {
    let mut reasons = validate_policy(policy);
    reasons.extend(validate_evidence(evidence));

    let metrics = quality_gate_metrics(evidence);
    let cases_by_id = evidence
        .cases
        .iter()
        .map(|case| (case.case_id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let mut failing_cases = evidence
        .cases
        .iter()
        .filter(|case| case.status != QualityCaseStatus::Pass)
        .map(|case| case.case_id.clone())
        .collect::<BTreeSet<_>>();

    if metrics.success_rate < policy.min_success_rate {
        reasons.push(format!(
            "success_rate below policy: {:.4} < {:.4}",
            metrics.success_rate, policy.min_success_rate
        ));
    }
    if metrics.degraded_rate > policy.max_degraded_rate {
        reasons.push(format!(
            "degraded_rate above policy: {:.4} > {:.4}",
            metrics.degraded_rate, policy.max_degraded_rate
        ));
    }
    if metrics.trace_completeness < policy.min_trace_completeness {
        reasons.push(format!(
            "trace_completeness below policy: {:.4} < {:.4}",
            metrics.trace_completeness, policy.min_trace_completeness
        ));
    }
    if metrics.privacy_violations > policy.max_privacy_violations {
        reasons.push(format!(
            "privacy_violations above policy: {} > {}",
            metrics.privacy_violations, policy.max_privacy_violations
        ));
    }
    if metrics.status_regressions > policy.max_status_regressions {
        reasons.push(format!(
            "status_regressions above policy: {} > {}",
            metrics.status_regressions, policy.max_status_regressions
        ));
    }
    if metrics.budget_regressions > policy.max_budget_regressions {
        reasons.push(format!(
            "budget_regressions above policy: {} > {}",
            metrics.budget_regressions, policy.max_budget_regressions
        ));
    }
    check_regression_ratio(
        &mut reasons,
        "p95_duration_regression_ratio",
        metrics.p95_duration_regression_ratio,
        metrics.p95_duration_ms,
        evidence.baseline.p95_duration_ms,
        policy.max_p95_duration_regression_ratio,
    );
    check_regression_ratio(
        &mut reasons,
        "total_token_regression_ratio",
        metrics.total_token_regression_ratio,
        metrics.total_tokens,
        evidence.baseline.total_tokens,
        policy.max_total_token_regression_ratio,
    );
    check_regression_ratio(
        &mut reasons,
        "total_cost_regression_ratio",
        metrics.total_cost_regression_ratio,
        metrics.total_cost_microunits,
        evidence.baseline.total_cost_microunits,
        policy.max_total_cost_regression_ratio,
    );

    for case_id in &policy.required_critical_cases {
        match cases_by_id.get(case_id.as_str()) {
            None => {
                reasons.push(format!("required critical case is missing: {case_id}"));
                failing_cases.insert(case_id.clone());
            }
            Some(case) if case.status != QualityCaseStatus::Pass => {
                reasons.push(format!("required critical case did not pass: {case_id}"));
                failing_cases.insert(case_id.clone());
            }
            Some(case) if case.privacy_violations > 0 || case.trace_completeness < 1.0 => {
                reasons.push(format!(
                    "required critical case has incomplete safety evidence: {case_id}"
                ));
                failing_cases.insert(case_id.clone());
            }
            Some(_) => {}
        }
    }

    if policy.require_case_links {
        for case in &evidence.cases {
            if !has_value(case.run_id.as_deref())
                || !valid_trace_id(case.trace_id.as_deref())
                || !valid_fingerprint(case.task_contract_fingerprint.as_deref())
            {
                reasons.push(format!(
                    "case is missing a valid run_id, trace_id, or task contract fingerprint: {}",
                    case.case_id
                ));
                failing_cases.insert(case.case_id.clone());
            }
        }
    }
    if policy.require_provenance {
        for (name, value) in [
            (
                "eval_report_fingerprint",
                evidence.provenance.eval_report_fingerprint.as_str(),
            ),
            (
                "baseline_report_fingerprint",
                evidence.provenance.baseline_report_fingerprint.as_str(),
            ),
            (
                "regression_report_fingerprint",
                evidence.provenance.regression_report_fingerprint.as_str(),
            ),
            (
                "privacy_report_fingerprint",
                evidence.provenance.privacy_report_fingerprint.as_str(),
            ),
        ] {
            if !valid_fingerprint(Some(value)) {
                reasons.push(format!("invalid release evidence provenance: {name}"));
            }
        }
        if evidence.provenance.assembler_version.trim().is_empty() {
            reasons.push("release evidence assembler_version must not be empty".to_string());
        }
    }

    reasons.sort();
    reasons.dedup();
    let decision = if reasons.is_empty() {
        QualityGateDecisionStatus::Pass
    } else {
        QualityGateDecisionStatus::Fail
    };
    let evidence_links = evidence
        .cases
        .iter()
        .filter_map(|case| {
            Some((
                case.case_id.clone(),
                format!(
                    "run_id={};trace_id={}",
                    case.run_id.as_deref()?,
                    case.trace_id.as_deref()?
                ),
            ))
        })
        .collect();
    let evidence_fingerprint = serde_json::to_value(evidence)
        .map(|value| canonical_json_fingerprint(&value))
        .unwrap_or_else(|_| canonical_json_fingerprint(&Value::Null));

    QualityGateDecisionV1 {
        schema_version: QUALITY_GATE_DECISION_SCHEMA_VERSION.to_string(),
        decision,
        policy_id: policy.policy_id.clone(),
        evidence_id: evidence.evidence_id.clone(),
        evidence_fingerprint,
        reasons,
        failing_cases: failing_cases.into_iter().collect(),
        metrics,
        evidence_links,
    }
}

#[must_use]
pub fn quality_gate_metrics(evidence: &QualityGateEvidenceV1) -> QualityGateMetricsV1 {
    let total_cases = u64::try_from(evidence.cases.len()).unwrap_or(u64::MAX);
    let passed_cases = count_status(&evidence.cases, QualityCaseStatus::Pass);
    let degraded_cases = count_status(&evidence.cases, QualityCaseStatus::Degraded);
    let failed_cases = count_status(&evidence.cases, QualityCaseStatus::Fail);
    let duration_values = evidence
        .cases
        .iter()
        .map(|case| case.duration_ms)
        .collect::<Vec<_>>();
    let p95_duration_ms = percentile_nearest_rank(&duration_values, 95);
    let total_tokens = evidence.cases.iter().fold(0_u64, |total, case| {
        total.saturating_add(case.total_tokens())
    });
    let total_cost_microunits = evidence.cases.iter().fold(0_u64, |total, case| {
        total.saturating_add(case.cost_microunits)
    });
    let privacy_violations = evidence.cases.iter().fold(0_u64, |total, case| {
        total.saturating_add(case.privacy_violations)
    });
    let trace_completeness = if total_cases == 0 {
        0.0
    } else {
        evidence
            .cases
            .iter()
            .map(|case| case.trace_completeness)
            .sum::<f64>()
            / total_cases as f64
    };

    QualityGateMetricsV1 {
        total_cases,
        passed_cases,
        degraded_cases,
        failed_cases,
        success_rate: rate(passed_cases, total_cases),
        degraded_rate: rate(degraded_cases, total_cases),
        trace_completeness,
        privacy_violations,
        p95_duration_ms,
        total_tokens,
        total_cost_microunits,
        p95_duration_regression_ratio: regression_ratio(
            p95_duration_ms,
            evidence.baseline.p95_duration_ms,
        ),
        total_token_regression_ratio: regression_ratio(
            total_tokens,
            evidence.baseline.total_tokens,
        ),
        total_cost_regression_ratio: regression_ratio(
            total_cost_microunits,
            evidence.baseline.total_cost_microunits,
        ),
        status_regressions: evidence.regression.status_regressions,
        budget_regressions: evidence.regression.budget_regressions,
    }
}

/// Converts one legacy eval report case into the release-gate evidence contract.
/// The caller supplies the OTel trace and task-contract fingerprints because they
/// are intentionally not inferred from file paths or third-party trace IDs.
pub fn quality_case_evidence_from_report(
    case: &Value,
    trace_id: Option<String>,
    task_contract_fingerprint: Option<String>,
    privacy_violations: u64,
) -> Result<QualityCaseEvidenceV1, String> {
    let case_id = case
        .get("case_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "eval case is missing case_id".to_string())?;
    let status = match case
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("fail")
        .to_ascii_lowercase()
        .as_str()
    {
        "pass" | "passed" | "success" => QualityCaseStatus::Pass,
        "degraded" => QualityCaseStatus::Degraded,
        _ => QualityCaseStatus::Fail,
    };
    let cost = case.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
    if !cost.is_finite() || cost < 0.0 {
        return Err(format!("eval case has invalid cost: {case_id}"));
    }
    let cost_microunits = (cost * 1_000_000.0).round().min(u64::MAX as f64) as u64;
    let trace_completeness = case
        .get("trace_completeness")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| {
            if case
                .get("trace_check_ok")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                1.0
            } else {
                0.0
            }
        });
    let failure_reasons = case
        .get("failure_reasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect();

    Ok(QualityCaseEvidenceV1 {
        case_id: case_id.to_string(),
        status,
        score: case.get("score").and_then(Value::as_f64).unwrap_or(0.0),
        duration_ms: case
            .get("duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        input_tokens: case
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_tokens: case
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cost_microunits,
        privacy_violations,
        trace_completeness,
        run_id: case
            .get("run_id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        trace_id,
        task_contract_fingerprint,
        failure_reasons,
    })
}

#[must_use]
pub fn quality_regression_evidence_from_report(regression: &Value) -> QualityRegressionEvidenceV1 {
    let summary = regression.get("summary").unwrap_or(regression);
    QualityRegressionEvidenceV1 {
        status_regressions: summary
            .get("status_regressions")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        budget_regressions: summary
            .get("budget_regressions")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    }
}

fn validate_policy(policy: &QualityGatePolicyV1) -> Vec<String> {
    let mut reasons = Vec::new();
    if policy.schema_version != QUALITY_GATE_POLICY_SCHEMA_VERSION {
        reasons.push(format!(
            "unsupported policy schema_version: {}",
            policy.schema_version
        ));
    }
    if policy.policy_id.trim().is_empty() {
        reasons.push("policy_id must not be empty".to_string());
    }
    for (name, value) in [
        ("min_success_rate", policy.min_success_rate),
        ("max_degraded_rate", policy.max_degraded_rate),
        ("min_trace_completeness", policy.min_trace_completeness),
    ] {
        if !valid_unit_interval(value) {
            reasons.push(format!("{name} must be finite and between 0 and 1"));
        }
    }
    for (name, value) in [
        (
            "max_p95_duration_regression_ratio",
            policy.max_p95_duration_regression_ratio,
        ),
        (
            "max_total_token_regression_ratio",
            policy.max_total_token_regression_ratio,
        ),
        (
            "max_total_cost_regression_ratio",
            policy.max_total_cost_regression_ratio,
        ),
    ] {
        if !value.is_finite() || value < 0.0 {
            reasons.push(format!("{name} must be finite and non-negative"));
        }
    }
    reasons
}

fn validate_evidence(evidence: &QualityGateEvidenceV1) -> Vec<String> {
    let mut reasons = Vec::new();
    if evidence.schema_version != QUALITY_GATE_EVIDENCE_SCHEMA_VERSION {
        reasons.push(format!(
            "unsupported evidence schema_version: {}",
            evidence.schema_version
        ));
    }
    if evidence.evidence_id.trim().is_empty() {
        reasons.push("evidence_id must not be empty".to_string());
    }
    if evidence.generated_at_ms == 0 {
        reasons.push("generated_at_ms must be greater than zero".to_string());
    }
    if evidence.cases.is_empty() {
        reasons.push("quality gate evidence must contain at least one case".to_string());
    }
    if evidence.subject.candidate_id.trim().is_empty()
        || evidence.subject.eval_dataset_version.trim().is_empty()
    {
        reasons.push("candidate_id and eval_dataset_version must not be empty".to_string());
    }
    if !valid_fingerprint(Some(evidence.subject.eval_dataset_fingerprint.as_str()))
        || !valid_fingerprint(Some(evidence.subject.versions.config_fingerprint.as_str()))
    {
        reasons.push(
            "dataset and configuration fingerprints must be 64 lowercase hex characters"
                .to_string(),
        );
    }
    for (name, value) in [
        (
            "harness_version",
            evidence.subject.versions.harness_version.as_str(),
        ),
        (
            "agent_version",
            evidence.subject.versions.agent_version.as_str(),
        ),
        (
            "prompt_version",
            evidence.subject.versions.prompt_version.as_str(),
        ),
        (
            "skill_set_version",
            evidence.subject.versions.skill_set_version.as_str(),
        ),
        (
            "tool_set_version",
            evidence.subject.versions.tool_set_version.as_str(),
        ),
    ] {
        if value.trim().is_empty() {
            reasons.push(format!("{name} must not be empty"));
        }
    }
    if evidence.baseline.baseline_id.trim().is_empty() {
        reasons.push("baseline_id must not be empty".to_string());
    }
    if evidence.baseline.case_count != u64::try_from(evidence.cases.len()).unwrap_or(u64::MAX) {
        reasons.push(format!(
            "baseline case_count does not match current evidence: {} != {}",
            evidence.baseline.case_count,
            evidence.cases.len()
        ));
    }
    let mut seen = BTreeSet::new();
    for case in &evidence.cases {
        if case.case_id.trim().is_empty() {
            reasons.push("case_id must not be empty".to_string());
        } else if !seen.insert(case.case_id.as_str()) {
            reasons.push(format!("duplicate case_id: {}", case.case_id));
        }
        if !case.score.is_finite() {
            reasons.push(format!("case score must be finite: {}", case.case_id));
        }
        if !valid_unit_interval(case.trace_completeness) {
            reasons.push(format!(
                "case trace_completeness must be between 0 and 1: {}",
                case.case_id
            ));
        }
    }
    reasons
}

fn check_regression_ratio(
    reasons: &mut Vec<String>,
    name: &str,
    ratio: Option<f64>,
    current: u64,
    baseline: u64,
    maximum: f64,
) {
    match ratio {
        Some(value) if value > maximum => {
            reasons.push(format!("{name} above policy: {value:.4} > {maximum:.4}"))
        }
        None if current > baseline => reasons.push(format!(
            "{name} cannot be bounded because the baseline is zero while current is {current}"
        )),
        _ => {}
    }
}

fn count_status(cases: &[QualityCaseEvidenceV1], status: QualityCaseStatus) -> u64 {
    u64::try_from(cases.iter().filter(|case| case.status == status).count()).unwrap_or(u64::MAX)
}

fn percentile_nearest_rank(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = percentile.saturating_mul(sorted.len()).saturating_add(99) / 100;
    sorted[rank.max(1).saturating_sub(1).min(sorted.len() - 1)]
}

fn regression_ratio(current: u64, baseline: u64) -> Option<f64> {
    if baseline == 0 {
        return (current == 0).then_some(0.0);
    }
    Some((current as f64 - baseline as f64) / baseline as f64)
}

fn rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn valid_unit_interval(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn has_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

fn valid_trace_id(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value.len() == 32
            && value != "00000000000000000000000000000000"
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn valid_fingerprint(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

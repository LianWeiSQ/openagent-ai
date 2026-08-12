use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use openagent_telemetry::{VersionIdentity, canonical_json_fingerprint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    QUALITY_GATE_EVIDENCE_SCHEMA_VERSION, QualityEvidenceProvenanceV1, QualityGateBaselineV1,
    QualityGateEvidenceV1, QualityGateSubjectV1, quality_case_evidence_from_report,
    quality_regression_evidence_from_report,
};

pub const RELEASE_EVIDENCE_REQUEST_SCHEMA_VERSION: &str = "openharness.release_evidence.request.v1";
pub const PRIVACY_AUDIT_REPORT_SCHEMA_VERSION: &str = "openharness.privacy_audit.report.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseEvidenceAssemblyRequestV1 {
    pub schema_version: String,
    pub evidence_id: String,
    pub candidate_id: String,
    pub generated_at_ms: u64,
    pub eval_dataset_version: String,
    pub baseline_id: String,
    pub eval_report_path: String,
    pub baseline_report_path: String,
    pub regression_report_path: Option<String>,
    pub privacy_report_path: String,
    pub eval_dataset_manifest_path: String,
    pub session_root: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyAuditCaseV1 {
    pub case_id: String,
    pub scanner_version: String,
    #[serde(default)]
    pub violations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyAuditReportV1 {
    pub schema_version: String,
    pub audit_id: String,
    pub cases: Vec<PrivacyAuditCaseV1>,
}

pub fn assemble_release_evidence(
    request: &ReleaseEvidenceAssemblyRequestV1,
    base_dir: &Path,
) -> Result<QualityGateEvidenceV1, String> {
    validate_request(request)?;
    let eval_report = read_json(&resolve(base_dir, &request.eval_report_path))?;
    let baseline_report = read_json(&resolve(base_dir, &request.baseline_report_path))?;
    let regression_report = request
        .regression_report_path
        .as_deref()
        .map(|path| read_json(&resolve(base_dir, path)))
        .transpose()?
        .unwrap_or_else(|| json!({"summary": {}}));
    let privacy_value = read_json(&resolve(base_dir, &request.privacy_report_path))?;
    let privacy_report = serde_json::from_value::<PrivacyAuditReportV1>(privacy_value.clone())
        .map_err(|error| format!("invalid privacy report: {error}"))?;
    if privacy_report.schema_version != PRIVACY_AUDIT_REPORT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported privacy report schema_version: {}",
            privacy_report.schema_version
        ));
    }
    let dataset_manifest = read_json(&resolve(base_dir, &request.eval_dataset_manifest_path))?;
    if let Some(version) = dataset_manifest.get("version").and_then(Value::as_str)
        && version != request.eval_dataset_version
    {
        return Err(format!(
            "eval dataset version mismatch: manifest={version}, request={}",
            request.eval_dataset_version
        ));
    }
    let session_root = resolve(base_dir, &request.session_root);
    let result_values = report_results(&eval_report, "eval")?;
    let baseline_values = report_results(&baseline_report, "baseline")?;
    if result_values.len() != baseline_values.len() {
        return Err(format!(
            "baseline case count does not match eval report: {} != {}",
            baseline_values.len(),
            result_values.len()
        ));
    }
    let privacy_by_case = privacy_case_map(&privacy_report)?;
    let result_ids = result_values
        .iter()
        .filter_map(|case| case.get("case_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    if privacy_by_case
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != result_ids
    {
        return Err("privacy report case set does not match eval report".to_string());
    }

    let mut versions = None::<VersionIdentity>;
    let mut cases = Vec::with_capacity(result_values.len());
    let mut seen_cases = BTreeSet::new();
    let mut seen_runs = BTreeSet::new();
    for raw_case in &result_values {
        let case_id = required_string(raw_case, "case_id")?;
        if !seen_cases.insert(case_id.to_string()) {
            return Err(format!("duplicate eval case_id: {case_id}"));
        }
        let session_id = required_string(raw_case, "session_id")?;
        let run_id = required_string(raw_case, "run_id")?;
        if !seen_runs.insert((session_id.to_string(), run_id.to_string())) {
            return Err(format!(
                "multiple eval cases reference the same durable run: {session_id}/{run_id}"
            ));
        }
        validate_path_segment(session_id, "session_id")?;
        validate_path_segment(run_id, "run_id")?;
        let run_dir = session_root.join(session_id).join("runs").join(run_id);
        let run = read_json(&run_dir.join("run.json"))?;
        let contract = run
            .get("task_contract")
            .ok_or_else(|| format!("run {run_id} is missing task_contract"))?;
        if contract.get("session_id").and_then(Value::as_str) != Some(session_id)
            || contract.get("run_id").and_then(Value::as_str) != Some(run_id)
        {
            return Err(format!(
                "eval case {case_id} does not match its durable task contract"
            ));
        }
        let run_versions = serde_json::from_value::<VersionIdentity>(
            contract
                .get("versions")
                .cloned()
                .ok_or_else(|| format!("run {run_id} is missing versions"))?,
        )
        .map_err(|error| format!("run {run_id} has invalid versions: {error}"))?;
        if let Some(expected) = versions.as_ref() {
            if expected != &run_versions {
                return Err(format!(
                    "release evidence mixes candidate versions at case {case_id}"
                ));
            }
        } else {
            versions = Some(run_versions);
        }
        let trace_id = contract
            .get("trace")
            .and_then(|trace| trace.get("trace_id"))
            .and_then(Value::as_str)
            .filter(|value| valid_hex(value, 32))
            .ok_or_else(|| format!("run {run_id} has no valid trace_id"))?
            .to_string();
        let durable_completeness = durable_trace_completeness(&run_dir, &trace_id)?;
        let report_trace_ok = raw_case
            .get("trace_check_ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut enriched = raw_case.clone();
        let Some(object) = enriched.as_object_mut() else {
            return Err(format!("eval case is not an object: {case_id}"));
        };
        object.insert(
            "trace_completeness".to_string(),
            json!(if report_trace_ok {
                durable_completeness
            } else {
                0.0
            }),
        );
        let privacy_violations = u64::try_from(
            privacy_by_case
                .get(case_id)
                .map(|audit| audit.violations.len())
                .unwrap_or_default(),
        )
        .unwrap_or(u64::MAX);
        cases.push(quality_case_evidence_from_report(
            &enriched,
            Some(trace_id),
            Some(canonical_json_fingerprint(contract)),
            privacy_violations,
        )?);
    }
    cases.sort_by(|left, right| left.case_id.cmp(&right.case_id));
    let versions = versions.ok_or_else(|| "eval report contains no cases".to_string())?;
    let baseline = baseline_from_report(
        &request.baseline_id,
        &baseline_values,
        u64::try_from(cases.len()).unwrap_or(u64::MAX),
    )?;
    let provenance = QualityEvidenceProvenanceV1 {
        assembler_version: env!("CARGO_PKG_VERSION").to_string(),
        eval_report_fingerprint: canonical_json_fingerprint(&eval_report),
        baseline_report_fingerprint: canonical_json_fingerprint(&baseline_report),
        regression_report_fingerprint: canonical_json_fingerprint(&regression_report),
        privacy_report_fingerprint: canonical_json_fingerprint(&privacy_value),
    };

    Ok(QualityGateEvidenceV1 {
        schema_version: QUALITY_GATE_EVIDENCE_SCHEMA_VERSION.to_string(),
        evidence_id: request.evidence_id.clone(),
        generated_at_ms: request.generated_at_ms,
        subject: QualityGateSubjectV1 {
            candidate_id: request.candidate_id.clone(),
            versions,
            eval_dataset_version: request.eval_dataset_version.clone(),
            eval_dataset_fingerprint: canonical_json_fingerprint(&dataset_manifest),
        },
        provenance,
        cases,
        baseline,
        regression: quality_regression_evidence_from_report(&regression_report),
    })
}

fn validate_request(request: &ReleaseEvidenceAssemblyRequestV1) -> Result<(), String> {
    if request.schema_version != RELEASE_EVIDENCE_REQUEST_SCHEMA_VERSION {
        return Err(format!(
            "unsupported release evidence request schema_version: {}",
            request.schema_version
        ));
    }
    for (name, value) in [
        ("evidence_id", request.evidence_id.as_str()),
        ("candidate_id", request.candidate_id.as_str()),
        (
            "eval_dataset_version",
            request.eval_dataset_version.as_str(),
        ),
        ("baseline_id", request.baseline_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{name} must not be empty"));
        }
    }
    if request.generated_at_ms == 0 {
        return Err("generated_at_ms must be greater than zero".to_string());
    }
    Ok(())
}

fn privacy_case_map(
    report: &PrivacyAuditReportV1,
) -> Result<BTreeMap<String, &PrivacyAuditCaseV1>, String> {
    if report.audit_id.trim().is_empty() {
        return Err("privacy audit_id must not be empty".to_string());
    }
    let mut cases = BTreeMap::new();
    for case in &report.cases {
        if case.case_id.trim().is_empty() || case.scanner_version.trim().is_empty() {
            return Err("privacy case_id and scanner_version must not be empty".to_string());
        }
        if cases.insert(case.case_id.clone(), case).is_some() {
            return Err(format!("duplicate privacy case_id: {}", case.case_id));
        }
    }
    Ok(cases)
}

fn baseline_from_report(
    baseline_id: &str,
    cases: &[Value],
    case_count: u64,
) -> Result<QualityGateBaselineV1, String> {
    let mut durations = Vec::with_capacity(cases.len());
    let mut total_tokens = 0_u64;
    let mut total_cost_microunits = 0_u64;
    for case in cases {
        durations.push(non_negative_number(case, "duration_ms")?);
        total_tokens = total_tokens
            .saturating_add(non_negative_number(case, "input_tokens")?)
            .saturating_add(non_negative_number(case, "output_tokens")?);
        let cost = case.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        if !cost.is_finite() || cost < 0.0 {
            return Err(format!(
                "baseline case {} has invalid cost",
                required_string(case, "case_id")?
            ));
        }
        total_cost_microunits = total_cost_microunits
            .saturating_add((cost * 1_000_000.0).round().min(u64::MAX as f64) as u64);
    }
    Ok(QualityGateBaselineV1 {
        baseline_id: baseline_id.to_string(),
        case_count,
        p95_duration_ms: nearest_rank_95(&durations),
        total_tokens,
        total_cost_microunits,
    })
}

fn durable_trace_completeness(run_dir: &Path, trace_id: &str) -> Result<f64, String> {
    let raw = fs::read_to_string(run_dir.join("events.jsonl"))
        .map_err(|error| format!("read durable events for {}: {error}", run_dir.display()))?;
    let mut has_run = false;
    let mut has_step = false;
    let mut has_effect = false;
    for event in raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        if event.get("trace_id").and_then(Value::as_str) != Some(trace_id)
            || !event
                .get("span_id")
                .and_then(Value::as_str)
                .is_some_and(|value| valid_hex(value, 16))
        {
            continue;
        }
        match event.get("event").and_then(Value::as_str) {
            Some("agent.run.finished") => has_run = true,
            Some("step.finished") => has_step = true,
            Some("provider.request.finished" | "tool.execute.finished") => has_effect = true,
            _ => {}
        }
    }
    Ok(f64::from(u8::from(has_run) + u8::from(has_step) + u8::from(has_effect)) / 3.0)
}

fn report_results(report: &Value, name: &str) -> Result<Vec<Value>, String> {
    report
        .get("results")
        .and_then(Value::as_array)
        .filter(|cases| !cases.is_empty())
        .cloned()
        .ok_or_else(|| format!("{name} report must contain non-empty results"))
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing required string field: {key}"))
}

fn non_negative_number(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        })
        .ok_or_else(|| format!("field {key} must be a non-negative integer"))
}

fn resolve(base_dir: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn validate_path_segment(value: &str, name: &str) -> Result<(), String> {
    if value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(format!("invalid {name} path segment"));
    }
    Ok(())
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && !value.bytes().all(|byte| byte == b'0')
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn nearest_rank_95(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = 95_usize.saturating_mul(sorted.len()).saturating_add(99) / 100;
    sorted[rank.max(1).saturating_sub(1).min(sorted.len() - 1)]
}

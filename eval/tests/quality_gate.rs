use std::{
    error::Error,
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use openagent_eval::{
    QUALITY_GATE_DECISION_SCHEMA_VERSION, QUALITY_GATE_EVIDENCE_SCHEMA_VERSION,
    QualityCaseEvidenceV1, QualityCaseStatus, QualityEvidenceProvenanceV1, QualityGateBaselineV1,
    QualityGateDecisionStatus, QualityGateEvidenceV1, QualityGatePolicyV1, QualityGateSubjectV1,
    QualityRegressionEvidenceV1, evaluate_quality_gate, quality_case_evidence_from_report,
    quality_regression_evidence_from_report,
};
use openagent_telemetry::VersionIdentity;

const CRITICAL_CASES: [&str; 6] = [
    "runtime.cancel",
    "runtime.deadline",
    "runtime.provider-retry",
    "runtime.tool-permission-deny",
    "recovery.restart",
    "telemetry.collector-outage",
];

#[test]
fn release_gate_passes_complete_versioned_evidence() {
    let policy = strict_policy();
    let evidence = passing_evidence();
    let decision = evaluate_quality_gate(&policy, &evidence);

    assert_eq!(
        decision.schema_version,
        QUALITY_GATE_DECISION_SCHEMA_VERSION
    );
    assert_eq!(decision.decision, QualityGateDecisionStatus::Pass);
    assert!(decision.reasons.is_empty());
    assert_eq!(decision.metrics.total_cases, 6);
    assert_eq!(decision.metrics.success_rate, 1.0);
    assert_eq!(decision.metrics.trace_completeness, 1.0);
    assert_eq!(decision.evidence_fingerprint.len(), 64);
    assert_eq!(decision.evidence_links.len(), 6);
}

#[test]
fn checked_in_policy_and_legacy_report_adapters_follow_the_gate_contract() {
    let policy: QualityGatePolicyV1 =
        serde_json::from_str(include_str!("../policies/release-v1.json"))
            .expect("checked-in policy");
    assert_eq!(
        policy.required_critical_cases,
        CRITICAL_CASES.into_iter().map(str::to_string).collect()
    );
    let case = serde_json::json!({
        "case_id": "runtime.deadline",
        "status": "pass",
        "score": 0.98,
        "duration_ms": 125,
        "input_tokens": 70,
        "output_tokens": 30,
        "cost": 0.00042,
        "run_id": "run-deadline",
        "trace_check_ok": true,
        "failure_reasons": []
    });
    let converted =
        quality_case_evidence_from_report(&case, Some("1".repeat(32)), Some("c".repeat(64)), 0)
            .expect("convert eval case");
    assert_eq!(converted.status, QualityCaseStatus::Pass);
    assert_eq!(converted.total_tokens(), 100);
    assert_eq!(converted.cost_microunits, 420);
    assert_eq!(converted.trace_completeness, 1.0);

    assert_eq!(
        quality_regression_evidence_from_report(&serde_json::json!({
            "summary": {"status_regressions": 2, "budget_regressions": 3}
        })),
        QualityRegressionEvidenceV1 {
            status_regressions: 2,
            budget_regressions: 3,
        }
    );
}

#[test]
fn release_gate_fails_critical_safety_and_budget_regressions() {
    let policy = strict_policy();
    let mut evidence = passing_evidence();
    evidence.cases[0].status = QualityCaseStatus::Fail;
    evidence.cases[0].privacy_violations = 1;
    evidence.cases[0].trace_completeness = 0.5;
    evidence.cases[0].run_id = None;
    evidence.cases[1].duration_ms = 200;
    evidence.cases[1].input_tokens = 260;
    evidence.cases[1].cost_microunits = 300;
    evidence.regression.status_regressions = 1;
    evidence.regression.budget_regressions = 1;

    let decision = evaluate_quality_gate(&policy, &evidence);
    assert_eq!(decision.decision, QualityGateDecisionStatus::Fail);
    for expected in [
        "required critical case did not pass",
        "privacy_violations above policy",
        "trace_completeness below policy",
        "status_regressions above policy",
        "budget_regressions above policy",
        "p95_duration_regression_ratio above policy",
        "total_token_regression_ratio above policy",
        "total_cost_regression_ratio above policy",
        "missing a valid run_id",
    ] {
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| reason.contains(expected)),
            "missing reason containing {expected:?}: {:?}",
            decision.reasons
        );
    }
    assert!(
        decision
            .failing_cases
            .contains(&"runtime.cancel".to_string())
    );
}

#[test]
fn release_gate_rejects_duplicate_cases_and_unversioned_subjects() {
    let policy = strict_policy();
    let mut evidence = passing_evidence();
    evidence.subject.versions.prompt_version.clear();
    evidence.subject.eval_dataset_fingerprint = "not-a-fingerprint".to_string();
    evidence.cases.push(evidence.cases[0].clone());

    let decision = evaluate_quality_gate(&policy, &evidence);
    assert_eq!(decision.decision, QualityGateDecisionStatus::Fail);
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| reason == "prompt_version must not be empty")
    );
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| reason.contains("dataset and configuration fingerprints"))
    );
    assert!(
        decision
            .reasons
            .iter()
            .any(|reason| reason.contains("duplicate case_id"))
    );
}

#[test]
fn quality_gate_cli_writes_decision_and_returns_release_exit_code() -> Result<(), Box<dyn Error>> {
    let temp = std::env::temp_dir().join(format!(
        "openharness-quality-gate-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir_all(&temp)?;
    let policy_path = temp.join("policy.json");
    let evidence_path = temp.join("evidence.json");
    let decision_path = temp.join("decision.json");
    fs::write(&policy_path, serde_json::to_vec_pretty(&strict_policy())?)?;
    fs::write(
        &evidence_path,
        serde_json::to_vec_pretty(&passing_evidence())?,
    )?;

    let status = Command::new(env!("CARGO_BIN_EXE_openharness-quality-gate"))
        .args([&policy_path, &evidence_path, &decision_path])
        .status()?;
    assert!(status.success());
    let decision: serde_json::Value = serde_json::from_slice(&fs::read(&decision_path)?)?;
    assert_eq!(decision["decision"], "pass");

    let mut failing = passing_evidence();
    failing.cases[0].privacy_violations = 1;
    fs::write(&evidence_path, serde_json::to_vec_pretty(&failing)?)?;
    let status = Command::new(env!("CARGO_BIN_EXE_openharness-quality-gate"))
        .args([&policy_path, &evidence_path, &decision_path])
        .status()?;
    assert_eq!(status.code(), Some(2));
    let decision: serde_json::Value = serde_json::from_slice(&fs::read(&decision_path)?)?;
    assert_eq!(decision["decision"], "fail");

    fs::remove_dir_all(temp)?;
    Ok(())
}

fn strict_policy() -> QualityGatePolicyV1 {
    QualityGatePolicyV1 {
        min_success_rate: 1.0,
        max_degraded_rate: 0.0,
        min_trace_completeness: 1.0,
        required_critical_cases: CRITICAL_CASES.into_iter().map(str::to_string).collect(),
        ..QualityGatePolicyV1::default()
    }
}

fn passing_evidence() -> QualityGateEvidenceV1 {
    let cases = CRITICAL_CASES
        .iter()
        .enumerate()
        .map(|(index, case_id)| QualityCaseEvidenceV1 {
            case_id: (*case_id).to_string(),
            status: QualityCaseStatus::Pass,
            score: 1.0,
            duration_ms: 100,
            input_tokens: 60,
            output_tokens: 40,
            cost_microunits: 100,
            privacy_violations: 0,
            trace_completeness: 1.0,
            run_id: Some(format!("run-{index}")),
            trace_id: Some(format!("{:032x}", index + 1)),
            task_contract_fingerprint: Some("c".repeat(64)),
            failure_reasons: Vec::new(),
        })
        .collect();
    QualityGateEvidenceV1 {
        schema_version: QUALITY_GATE_EVIDENCE_SCHEMA_VERSION.to_string(),
        evidence_id: "release-candidate-42-evidence".to_string(),
        generated_at_ms: 1_786_112_000_000,
        subject: QualityGateSubjectV1 {
            candidate_id: "release-candidate-42".to_string(),
            versions: VersionIdentity {
                harness_version: "0.1.0+42".to_string(),
                agent_version: "agent-v7".to_string(),
                prompt_version: "prompt-v11".to_string(),
                skill_set_version: "skills-v4".to_string(),
                tool_set_version: "tools-v9".to_string(),
                config_fingerprint: "a".repeat(64),
            },
            eval_dataset_version: "release-suite-v3".to_string(),
            eval_dataset_fingerprint: "b".repeat(64),
        },
        provenance: QualityEvidenceProvenanceV1 {
            assembler_version: "0.1.0".to_string(),
            eval_report_fingerprint: "d".repeat(64),
            baseline_report_fingerprint: "e".repeat(64),
            regression_report_fingerprint: "f".repeat(64),
            privacy_report_fingerprint: "1".repeat(64),
        },
        cases,
        baseline: QualityGateBaselineV1 {
            baseline_id: "main-2026-08-01".to_string(),
            case_count: 6,
            p95_duration_ms: 100,
            total_tokens: 600,
            total_cost_microunits: 600,
        },
        regression: QualityRegressionEvidenceV1::default(),
    }
}

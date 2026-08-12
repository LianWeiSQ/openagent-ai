use std::{
    error::Error,
    fs,
    path::Path,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use openagent_eval::{
    PRIVACY_AUDIT_REPORT_SCHEMA_VERSION, RELEASE_EVIDENCE_REQUEST_SCHEMA_VERSION,
    ReleaseEvidenceAssemblyRequestV1, assemble_release_evidence,
};
use serde_json::{Value, json};

#[test]
fn assembler_correlates_eval_privacy_run_trace_and_baseline() -> Result<(), Box<dyn Error>> {
    let root = fixture_root()?;
    let request = write_fixture(&root)?;
    let evidence = assemble_release_evidence(&request, &root)?;

    assert_eq!(evidence.cases.len(), 2);
    assert_eq!(evidence.subject.versions.agent_version, "agent-v7");
    assert_eq!(evidence.subject.eval_dataset_version, "suite-v1");
    assert_eq!(evidence.baseline.case_count, 2);
    assert_eq!(evidence.baseline.p95_duration_ms, 110);
    assert_eq!(evidence.baseline.total_tokens, 190);
    assert_eq!(evidence.baseline.total_cost_microunits, 300);
    assert!(
        evidence
            .cases
            .iter()
            .all(|case| case.trace_completeness == 1.0)
    );
    assert_eq!(evidence.cases[0].privacy_violations, 0);
    assert_eq!(evidence.provenance.eval_report_fingerprint.len(), 64);
    assert_eq!(evidence.provenance.privacy_report_fingerprint.len(), 64);

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn assembler_fails_closed_for_missing_privacy_or_mixed_versions() -> Result<(), Box<dyn Error>> {
    let root = fixture_root()?;
    let request = write_fixture(&root)?;
    write_json(
        &root.join("privacy.json"),
        &json!({
            "schema_version": PRIVACY_AUDIT_REPORT_SCHEMA_VERSION,
            "audit_id": "privacy-1",
            "cases": [{"case_id": "case-a", "scanner_version": "scanner-v1", "violations": []}]
        }),
    )?;
    assert!(
        assemble_release_evidence(&request, &root)
            .expect_err("missing privacy case must fail")
            .contains("case set")
    );

    write_privacy(&root)?;
    let run_path = root.join("sessions/session-b/runs/run-b/run.json");
    let mut run: Value = serde_json::from_slice(&fs::read(&run_path)?)?;
    run["task_contract"]["versions"]["prompt_version"] = json!("prompt-v12");
    write_json(&run_path, &run)?;
    assert!(
        assemble_release_evidence(&request, &root)
            .expect_err("mixed versions must fail")
            .contains("mixes candidate versions")
    );

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn evidence_cli_resolves_request_relative_paths() -> Result<(), Box<dyn Error>> {
    let root = fixture_root()?;
    let request = write_fixture(&root)?;
    let request_path = root.join("request.json");
    let evidence_path = root.join("evidence.json");
    write_json(&request_path, &serde_json::to_value(request)?)?;

    let status = Command::new(env!("CARGO_BIN_EXE_openharness-evidence"))
        .arg(&request_path)
        .arg(&evidence_path)
        .status()?;
    assert!(status.success());
    let evidence: Value = serde_json::from_slice(&fs::read(&evidence_path)?)?;
    assert_eq!(evidence["cases"].as_array().map(Vec::len), Some(2));
    assert_eq!(evidence["subject"]["candidate_id"], "candidate-42");

    fs::remove_dir_all(root)?;
    Ok(())
}

fn write_fixture(root: &Path) -> Result<ReleaseEvidenceAssemblyRequestV1, Box<dyn Error>> {
    let results = vec![
        eval_case("case-a", "session-a", "run-a", 120, 100, 0.0002),
        eval_case("case-b", "session-b", "run-b", 90, 80, 0.0001),
    ];
    let baseline = vec![
        eval_case("case-a", "baseline-a", "baseline-run-a", 100, 90, 0.0002),
        eval_case("case-b", "baseline-b", "baseline-run-b", 110, 100, 0.0001),
    ];
    write_json(&root.join("report.json"), &json!({"results": results}))?;
    write_json(&root.join("baseline.json"), &json!({"results": baseline}))?;
    write_json(
        &root.join("regression.json"),
        &json!({"summary": {"status_regressions": 0, "budget_regressions": 0}}),
    )?;
    write_json(
        &root.join("dataset.json"),
        &json!({"version": "suite-v1", "cases": ["case-a", "case-b"]}),
    )?;
    write_privacy(root)?;
    write_run(root, "session-a", "run-a", "1")?;
    write_run(root, "session-b", "run-b", "2")?;
    Ok(ReleaseEvidenceAssemblyRequestV1 {
        schema_version: RELEASE_EVIDENCE_REQUEST_SCHEMA_VERSION.to_string(),
        evidence_id: "evidence-42".to_string(),
        candidate_id: "candidate-42".to_string(),
        generated_at_ms: 1_786_115_000_000,
        eval_dataset_version: "suite-v1".to_string(),
        baseline_id: "main-41".to_string(),
        eval_report_path: "report.json".to_string(),
        baseline_report_path: "baseline.json".to_string(),
        regression_report_path: Some("regression.json".to_string()),
        privacy_report_path: "privacy.json".to_string(),
        eval_dataset_manifest_path: "dataset.json".to_string(),
        session_root: "sessions".to_string(),
    })
}

fn eval_case(
    case_id: &str,
    session_id: &str,
    run_id: &str,
    duration_ms: u64,
    total_tokens: u64,
    cost: f64,
) -> Value {
    json!({
        "case_id": case_id,
        "status": "pass",
        "score": 1.0,
        "duration_ms": duration_ms,
        "steps": 1,
        "tool_calls": 0,
        "input_tokens": total_tokens.saturating_sub(20),
        "output_tokens": 20,
        "cost": cost,
        "failure_reasons": [],
        "trace_check_ok": true,
        "session_id": session_id,
        "run_id": run_id
    })
}

fn write_privacy(root: &Path) -> Result<(), Box<dyn Error>> {
    write_json(
        &root.join("privacy.json"),
        &json!({
            "schema_version": PRIVACY_AUDIT_REPORT_SCHEMA_VERSION,
            "audit_id": "privacy-1",
            "cases": [
                {"case_id": "case-a", "scanner_version": "scanner-v1", "violations": []},
                {"case_id": "case-b", "scanner_version": "scanner-v1", "violations": []}
            ]
        }),
    )
}

fn write_run(
    root: &Path,
    session_id: &str,
    run_id: &str,
    trace_digit: &str,
) -> Result<(), Box<dyn Error>> {
    let run_dir = root
        .join("sessions")
        .join(session_id)
        .join("runs")
        .join(run_id);
    fs::create_dir_all(&run_dir)?;
    let trace_id = trace_digit.repeat(32);
    write_json(
        &run_dir.join("run.json"),
        &json!({
            "session_id": session_id,
            "run_id": run_id,
            "task_contract": {
                "session_id": session_id,
                "run_id": run_id,
                "trace": {"trace_id": trace_id, "span_id": "a".repeat(16)},
                "versions": {
                    "harness_version": "0.1.0+42",
                    "agent_version": "agent-v7",
                    "prompt_version": "prompt-v11",
                    "skill_set_version": "skills-v4",
                    "tool_set_version": "tools-v9",
                    "config_fingerprint": "b".repeat(64)
                }
            }
        }),
    )?;
    let events = [
        json!({"event": "agent.run.finished", "trace_id": trace_id, "span_id": "1".repeat(16)}),
        json!({"event": "step.finished", "trace_id": trace_id, "span_id": "2".repeat(16)}),
        json!({"event": "provider.request.finished", "trace_id": trace_id, "span_id": "3".repeat(16)}),
    ];
    let raw = events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    fs::write(run_dir.join("events.jsonl"), format!("{raw}\n"))?;
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn fixture_root() -> Result<std::path::PathBuf, Box<dyn Error>> {
    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "openharness-evidence-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root)?;
    Ok(root)
}

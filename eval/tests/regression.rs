use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
    process::Command,
    time::SystemTime,
};

use openagent_eval::{
    BadCaseAssertionV1, BadCaseReplayV1, BadCaseSourceV1,
    REGRESSION_FIXTURE_ARTIFACT_SCHEMA_VERSION, REGRESSION_FIXTURE_SCHEMA_VERSION,
    REGRESSION_OBSERVATIONS_SCHEMA_VERSION, RegressionFixtureArtifactV1, RegressionFixtureV1,
    RegressionObservationSetV1, build_regression_dataset_manifest, replay_regression_dataset,
    validate_regression_dataset_manifest, validate_regression_observations,
};
use openagent_telemetry::{VersionIdentity, canonical_json_fingerprint};
use serde_json::{Value, json};

#[test]
fn regression_dataset_is_content_addressed_and_replays_deterministic_assertions() {
    let root = temp_root("openharness-regression-core");
    fs::create_dir_all(&root).expect("dataset root");
    let fixture_path = root.join("deadline.json");
    let fixture = fixture_artifact();
    write_json(&fixture_path, &fixture);
    let manifest = build_regression_dataset_manifest(
        "release-regressions",
        "release-suite-v4",
        &root,
        std::slice::from_ref(&fixture_path),
    )
    .expect("build manifest");
    let loaded = validate_regression_dataset_manifest(&manifest, &root).expect("valid dataset");
    assert_eq!(loaded, vec![fixture]);
    assert_eq!(manifest.dataset_fingerprint.len(), 64);

    let observations = RegressionObservationSetV1 {
        schema_version: REGRESSION_OBSERVATIONS_SCHEMA_VERSION.to_string(),
        observations: BTreeMap::from([(
            "runtime.deadline.bad-case-42".to_string(),
            json!({
                "session_id": "replay-session",
                "turn_id": "replay-run",
                "trace_id": "3".repeat(32),
                "reason_code": "deadline_exceeded",
                "events": [{"status": "failed"}],
                "usage": {"total_tokens": 0}
            }),
        )]),
    };
    validate_regression_observations(&observations, &loaded).expect("matching observations");
    let report = replay_regression_dataset(&manifest, &root, "test_observation", |fixture| {
        Ok(observations.observations[&fixture.fixture_id].clone())
    })
    .expect("replay");
    assert!(report.passed);
    assert_eq!(report.passed_cases, 1);
    assert_eq!(report.cases[0].run_id.as_deref(), Some("replay-run"));
    assert_eq!(report.cases[0].assertions.len(), 3);
    assert!(
        report.cases[0]
            .assertions
            .iter()
            .all(|assertion| assertion.passed)
    );

    let failed = replay_regression_dataset(&manifest, &root, "test_observation", |_| {
        Ok(json!({"reason_code": "success", "usage": {"total_tokens": 10}}))
    })
    .expect("failed replay report");
    assert!(!failed.passed);
    assert_eq!(failed.failed_cases, 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn regression_dataset_rejects_manifest_tampering_and_observation_drift() {
    let root = temp_root("openharness-regression-tamper");
    fs::create_dir_all(&root).expect("dataset root");
    let fixture_path = root.join("deadline.json");
    write_json(&fixture_path, &fixture_artifact());
    let mut manifest = build_regression_dataset_manifest(
        "release-regressions",
        "release-suite-v4",
        &root,
        &[fixture_path],
    )
    .expect("build manifest");
    manifest.fixtures[0].content_fingerprint = "f".repeat(64);
    assert!(
        validate_regression_dataset_manifest(&manifest, &root)
            .expect_err("tampered manifest")
            .contains("fingerprint mismatch")
    );

    let valid = build_regression_dataset_manifest(
        "release-regressions",
        "release-suite-v4",
        &root,
        &[root.join("deadline.json")],
    )
    .expect("valid manifest");
    let fixtures = validate_regression_dataset_manifest(&valid, &root).expect("fixtures");
    let observations = RegressionObservationSetV1 {
        schema_version: REGRESSION_OBSERVATIONS_SCHEMA_VERSION.to_string(),
        observations: BTreeMap::new(),
    };
    assert!(validate_regression_observations(&observations, &fixtures).is_err());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn regression_cli_indexes_validates_and_replays_observations() -> Result<(), Box<dyn Error>> {
    let root = temp_root("openharness-regression-cli");
    fs::create_dir_all(&root)?;
    let fixture_path = root.join("deadline.json");
    let manifest_path = root.join("dataset.json");
    let observations_path = root.join("observations.json");
    let report_path = root.join("report.json");
    write_json(&fixture_path, &fixture_artifact());
    write_json(
        &observations_path,
        &RegressionObservationSetV1 {
            schema_version: REGRESSION_OBSERVATIONS_SCHEMA_VERSION.to_string(),
            observations: BTreeMap::from([(
                "runtime.deadline.bad-case-42".to_string(),
                json!({
                    "reason_code": "deadline_exceeded",
                    "events": [{"status": "failed"}],
                    "usage": {"total_tokens": 0}
                }),
            )]),
        },
    );
    let binary = env!("CARGO_BIN_EXE_openharness-regression");
    assert!(
        Command::new(binary)
            .args(["index", "release-regressions", "release-suite-v4"])
            .arg(&manifest_path)
            .arg(&fixture_path)
            .status()?
            .success()
    );
    assert!(
        Command::new(binary)
            .args(["validate"])
            .arg(&manifest_path)
            .status()?
            .success()
    );
    assert!(
        Command::new(binary)
            .args(["replay-observations"])
            .arg(&manifest_path)
            .arg(&observations_path)
            .arg(&report_path)
            .status()?
            .success()
    );
    let report: Value = serde_json::from_str(&fs::read_to_string(&report_path)?)?;
    assert_eq!(report["passed"], true);
    fs::remove_dir_all(root)?;
    Ok(())
}

fn fixture_artifact() -> RegressionFixtureArtifactV1 {
    let fixture = RegressionFixtureV1 {
        schema_version: REGRESSION_FIXTURE_SCHEMA_VERSION.to_string(),
        fixture_id: "runtime.deadline.bad-case-42".to_string(),
        dataset_version: "release-suite-v4".to_string(),
        promoted_at_ms: 105,
        source_bad_case_id: "bad-case-42".to_string(),
        source_bad_case_fingerprint: "b".repeat(64),
        source: BadCaseSourceV1 {
            session_id: "source-session".to_string(),
            run_id: "source-run".to_string(),
            trace_id: "1".repeat(32),
            task_contract_fingerprint: "2".repeat(64),
            versions: VersionIdentity {
                harness_version: "0.1.0+42".to_string(),
                agent_version: "agent-v7".to_string(),
                prompt_version: "prompt-v11".to_string(),
                skill_set_version: "skills-v4".to_string(),
                tool_set_version: "tools-v9".to_string(),
                config_fingerprint: "a".repeat(64),
            },
            outcome: "failed".to_string(),
            reason_code: "deadline_exceeded".to_string(),
        },
        replay: BadCaseReplayV1 {
            input: json!({"input": "do not call provider"}),
            runtime_overrides: json!({"deadline_at_ms": 1}),
            expected_assertions: vec![
                BadCaseAssertionV1 {
                    kind: "json_path".to_string(),
                    path: "$.reason_code".to_string(),
                    operator: "eq".to_string(),
                    expected: json!("deadline_exceeded"),
                },
                BadCaseAssertionV1 {
                    kind: "json_path".to_string(),
                    path: "$.events[0].status".to_string(),
                    operator: "contains".to_string(),
                    expected: json!("fail"),
                },
                BadCaseAssertionV1 {
                    kind: "json_path".to_string(),
                    path: "$.usage.total_tokens".to_string(),
                    operator: "lte".to_string(),
                    expected: json!(0),
                },
            ],
            tags: BTreeSet::from(["deadline".to_string(), "runtime".to_string()]),
        },
    };
    let content_fingerprint =
        canonical_json_fingerprint(&serde_json::to_value(&fixture).expect("serialize fixture"));
    RegressionFixtureArtifactV1 {
        schema_version: REGRESSION_FIXTURE_ARTIFACT_SCHEMA_VERSION.to_string(),
        content_fingerprint,
        fixture,
    }
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).expect("serialize")
        ),
    )
    .expect("write json");
}

fn temp_root(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

use std::{error::Error, fs, process::Command, time::SystemTime};

use openagent_eval::{
    BAD_CASE_CAPTURE_SCHEMA_VERSION, BadCaseAssertionV1, BadCaseCaptureInputV1, BadCaseCategory,
    BadCaseReplayV1, BadCaseSeverity, BadCaseState, capture_bad_case, promote_bad_case_to_fixture,
    transition_bad_case, validate_bad_case_artifact, validate_regression_fixture_artifact,
};
use serde_json::json;

#[test]
fn failed_run_becomes_sanitized_traceable_bad_case() {
    let source = openagent_eval::bad_case_source_from_run(&run_record()).expect("run source");
    let artifact = capture_bad_case(capture_input(source)).expect("capture bad case");

    assert_eq!(artifact.record.state, BadCaseState::Captured);
    assert!(artifact.record.privacy_scan_passed);
    assert_eq!(artifact.content_fingerprint.len(), 64);
    assert_eq!(artifact.record.source.task_contract_fingerprint.len(), 64);
    assert_eq!(artifact.record.replay.input["api_key"], "[REDACTED]");
    assert_eq!(
        artifact.record.replay.input["headers"]["authorization"],
        "[REDACTED]"
    );
    assert_eq!(
        artifact.record.replay.input["prompt"],
        "call with [REDACTED]"
    );
    assert!(artifact.record.redacted_paths.len() >= 3);
    validate_bad_case_artifact(&artifact).expect("valid artifact");
}

#[test]
fn bad_case_requires_ordered_verification_before_fixture_promotion() {
    let source = openagent_eval::bad_case_source_from_run(&run_record()).expect("run source");
    let captured = capture_bad_case(capture_input(source)).expect("capture bad case");
    assert!(transition_bad_case(&captured, BadCaseState::Fixed, "owner", "skip", 101).is_err());

    let triaged = transition_bad_case(
        &captured,
        BadCaseState::Triaged,
        "runtime-team",
        "classified",
        101,
    )
    .expect("triage");
    let fixture_ready = transition_bad_case(
        &triaged,
        BadCaseState::FixtureReady,
        "runtime-team",
        "assertion added",
        102,
    )
    .expect("fixture ready");
    let fixed = transition_bad_case(
        &fixture_ready,
        BadCaseState::Fixed,
        "runtime-team",
        "deadline checked before provider",
        103,
    )
    .expect("fixed");
    let verified = transition_bad_case(
        &fixed,
        BadCaseState::Verified,
        "eval-team",
        "replay passed",
        104,
    )
    .expect("verified");
    let (promoted, fixture) = promote_bad_case_to_fixture(
        &verified,
        "runtime.deadline.bad-case-42",
        "release-suite-v4",
        "eval-team",
        105,
    )
    .expect("promote");

    assert_eq!(promoted.record.state, BadCaseState::Promoted);
    assert_eq!(fixture.fixture.source_bad_case_id, "bad-case-42");
    assert_eq!(fixture.fixture.dataset_version, "release-suite-v4");
    assert_eq!(fixture.fixture.replay.expected_assertions.len(), 1);
    validate_bad_case_artifact(&promoted).expect("promoted artifact");
    validate_regression_fixture_artifact(&fixture).expect("regression fixture");

    let mut tampered = fixture;
    tampered.fixture.replay.input["input"] = json!("changed");
    assert!(validate_regression_fixture_artifact(&tampered).is_err());
}

#[test]
fn bad_case_cli_captures_and_validates_artifact() -> Result<(), Box<dyn Error>> {
    let root = std::env::temp_dir().join(format!(
        "openharness-bad-case-{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::create_dir_all(&root)?;
    let capture_path = root.join("capture.json");
    let artifact_path = root.join("bad-case.json");
    let source = openagent_eval::bad_case_source_from_run(&run_record())?;
    fs::write(
        &capture_path,
        serde_json::to_vec_pretty(&capture_input(source))?,
    )?;

    let binary = env!("CARGO_BIN_EXE_openharness-bad-case");
    assert!(
        Command::new(binary)
            .args(["capture"])
            .arg(&capture_path)
            .arg(&artifact_path)
            .status()?
            .success()
    );
    assert!(
        Command::new(binary)
            .args(["validate"])
            .arg(&artifact_path)
            .status()?
            .success()
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

fn run_record() -> serde_json::Value {
    json!({
        "session_id": "session-42",
        "run_id": "run-42",
        "outcome": "failed",
        "reason_code": "deadline_exceeded",
        "task_contract": {
            "schema_version": "openharness.task.v1",
            "session_id": "session-42",
            "run_id": "run-42",
            "trace": {"trace_id": "1".repeat(32), "span_id": "2".repeat(16)},
            "versions": {
                "harness_version": "0.1.0+42",
                "agent_version": "agent-v7",
                "prompt_version": "prompt-v11",
                "skill_set_version": "skills-v4",
                "tool_set_version": "tools-v9",
                "config_fingerprint": "a".repeat(64)
            }
        }
    })
}

fn capture_input(source: openagent_eval::BadCaseSourceV1) -> BadCaseCaptureInputV1 {
    BadCaseCaptureInputV1 {
        schema_version: BAD_CASE_CAPTURE_SCHEMA_VERSION.to_string(),
        bad_case_id: "bad-case-42".to_string(),
        captured_at_ms: 100,
        category: BadCaseCategory::DeadlineOrBudget,
        severity: BadCaseSeverity::High,
        title: "provider called after deadline".to_string(),
        owner: None,
        source,
        replay: BadCaseReplayV1 {
            input: json!({
                "input": "do not call provider",
                "prompt": "call with Bearer abcdefghijklmnop",
                "api_key": "sk-supersecret123",
                "headers": {"authorization": "Bearer abcdefghijklmnop"}
            }),
            runtime_overrides: json!({"deadline_at_ms": 99, "session_token": "token-value"}),
            expected_assertions: vec![BadCaseAssertionV1 {
                kind: "json_path".to_string(),
                path: "$.reason_code".to_string(),
                operator: "eq".to_string(),
                expected: json!("deadline_exceeded"),
            }],
            tags: ["runtime".to_string(), "deadline".to_string()]
                .into_iter()
                .collect(),
        },
    }
}

use std::collections::BTreeSet;

use openagent_telemetry::{VersionIdentity, canonical_json_fingerprint};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const BAD_CASE_CAPTURE_SCHEMA_VERSION: &str = "openharness.bad_case.capture.v1";
pub const BAD_CASE_SCHEMA_VERSION: &str = "openharness.bad_case.v1";
pub const BAD_CASE_ARTIFACT_SCHEMA_VERSION: &str = "openharness.bad_case.artifact.v1";
pub const REGRESSION_FIXTURE_SCHEMA_VERSION: &str = "openharness.regression_fixture.v1";
pub const REGRESSION_FIXTURE_ARTIFACT_SCHEMA_VERSION: &str =
    "openharness.regression_fixture.artifact.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadCaseCategory {
    RequirementMismatch,
    ProviderFailure,
    ToolFailure,
    PermissionFailure,
    DeadlineOrBudget,
    QualityRegression,
    SafetyOrPrivacy,
    RecoveryFailure,
    ObservabilityGap,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadCaseSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadCaseState {
    Captured,
    Triaged,
    FixtureReady,
    Fixed,
    Verified,
    Promoted,
    Rejected,
}

impl BadCaseState {
    fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Captured, Self::Triaged)
                | (Self::Triaged, Self::FixtureReady)
                | (Self::FixtureReady, Self::Fixed)
                | (Self::Fixed, Self::Verified)
                | (Self::Verified, Self::Promoted)
                | (_, Self::Rejected)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadCaseSourceV1 {
    pub session_id: String,
    pub run_id: String,
    pub trace_id: String,
    pub task_contract_fingerprint: String,
    pub versions: VersionIdentity,
    pub outcome: String,
    pub reason_code: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadCaseAssertionV1 {
    pub kind: String,
    pub path: String,
    pub operator: String,
    pub expected: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadCaseReplayV1 {
    pub input: Value,
    #[serde(default)]
    pub runtime_overrides: Value,
    #[serde(default)]
    pub expected_assertions: Vec<BadCaseAssertionV1>,
    #[serde(default)]
    pub tags: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadCaseCaptureInputV1 {
    pub schema_version: String,
    pub bad_case_id: String,
    pub captured_at_ms: u64,
    pub category: BadCaseCategory,
    pub severity: BadCaseSeverity,
    pub title: String,
    pub owner: Option<String>,
    pub source: BadCaseSourceV1,
    pub replay: BadCaseReplayV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadCaseTransitionV1 {
    pub from: BadCaseState,
    pub to: BadCaseState,
    pub at_ms: u64,
    pub owner: String,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadCaseRecordV1 {
    pub schema_version: String,
    pub bad_case_id: String,
    pub captured_at_ms: u64,
    pub updated_at_ms: u64,
    pub category: BadCaseCategory,
    pub severity: BadCaseSeverity,
    pub state: BadCaseState,
    pub title: String,
    pub owner: Option<String>,
    pub source: BadCaseSourceV1,
    pub replay: BadCaseReplayV1,
    pub redacted_paths: Vec<String>,
    pub privacy_scan_passed: bool,
    pub transitions: Vec<BadCaseTransitionV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadCaseArtifactV1 {
    pub schema_version: String,
    pub content_fingerprint: String,
    pub record: BadCaseRecordV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionFixtureV1 {
    pub schema_version: String,
    pub fixture_id: String,
    pub dataset_version: String,
    pub promoted_at_ms: u64,
    pub source_bad_case_id: String,
    pub source_bad_case_fingerprint: String,
    pub source: BadCaseSourceV1,
    pub replay: BadCaseReplayV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionFixtureArtifactV1 {
    pub schema_version: String,
    pub content_fingerprint: String,
    pub fixture: RegressionFixtureV1,
}

pub fn bad_case_source_from_run(run: &Value) -> Result<BadCaseSourceV1, String> {
    let contract = run
        .get("task_contract")
        .ok_or_else(|| "run is missing task_contract".to_string())?;
    let versions = serde_json::from_value::<VersionIdentity>(
        contract
            .get("versions")
            .cloned()
            .ok_or_else(|| "task contract is missing versions".to_string())?,
    )
    .map_err(|error| format!("invalid task contract versions: {error}"))?;
    let field = |name: &str| {
        run.get(name)
            .or_else(|| contract.get(name))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| format!("run is missing {name}"))
    };
    let trace_id = contract
        .get("trace")
        .and_then(|trace| trace.get("trace_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "task contract is missing trace_id".to_string())?
        .to_string();
    Ok(BadCaseSourceV1 {
        session_id: field("session_id")?,
        run_id: field("run_id")?,
        trace_id,
        task_contract_fingerprint: canonical_json_fingerprint(contract),
        versions,
        outcome: run
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        reason_code: run
            .get("reason_code")
            .or_else(|| run.get("finish_reason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

pub fn capture_bad_case(input: BadCaseCaptureInputV1) -> Result<BadCaseArtifactV1, String> {
    if input.schema_version != BAD_CASE_CAPTURE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported bad case capture schema_version: {}",
            input.schema_version
        ));
    }
    validate_artifact_id(&input.bad_case_id, "bad_case_id")?;
    validate_identity(&input.title, "title")?;
    validate_source(&input.source)?;
    if input.captured_at_ms == 0 {
        return Err("captured_at_ms must be greater than zero".to_string());
    }
    let mut redacted_paths = Vec::new();
    let replay = BadCaseReplayV1 {
        input: sanitize_value(&input.replay.input, "$.input", &mut redacted_paths),
        runtime_overrides: sanitize_value(
            &input.replay.runtime_overrides,
            "$.runtime_overrides",
            &mut redacted_paths,
        ),
        expected_assertions: input
            .replay
            .expected_assertions
            .into_iter()
            .map(|assertion| BadCaseAssertionV1 {
                expected: sanitize_value(
                    &assertion.expected,
                    "$.expected_assertions.expected",
                    &mut redacted_paths,
                ),
                ..assertion
            })
            .collect(),
        tags: input.replay.tags,
    };
    redacted_paths.sort();
    redacted_paths.dedup();
    let privacy_scan_passed = !contains_sensitive_material(
        &serde_json::to_value(&replay)
            .map_err(|error| format!("serialize sanitized replay: {error}"))?,
    );
    let record = BadCaseRecordV1 {
        schema_version: BAD_CASE_SCHEMA_VERSION.to_string(),
        bad_case_id: input.bad_case_id,
        captured_at_ms: input.captured_at_ms,
        updated_at_ms: input.captured_at_ms,
        category: input.category,
        severity: input.severity,
        state: BadCaseState::Captured,
        title: input.title,
        owner: input.owner,
        source: input.source,
        replay,
        redacted_paths,
        privacy_scan_passed,
        transitions: Vec::new(),
    };
    Ok(bad_case_artifact(record))
}

pub fn transition_bad_case(
    artifact: &BadCaseArtifactV1,
    next: BadCaseState,
    owner: &str,
    note: &str,
    at_ms: u64,
) -> Result<BadCaseArtifactV1, String> {
    validate_bad_case_artifact(artifact)?;
    validate_identity(owner, "owner")?;
    if at_ms < artifact.record.updated_at_ms {
        return Err("transition time must not move backwards".to_string());
    }
    if !artifact.record.state.can_transition_to(&next) {
        return Err(format!(
            "invalid bad case transition: {:?} -> {next:?}",
            artifact.record.state
        ));
    }
    if next != BadCaseState::Rejected && !artifact.record.privacy_scan_passed {
        return Err("bad case cannot advance while privacy scan is failing".to_string());
    }
    let mut record = artifact.record.clone();
    record.transitions.push(BadCaseTransitionV1 {
        from: record.state.clone(),
        to: next.clone(),
        at_ms,
        owner: owner.to_string(),
        note: note.to_string(),
    });
    record.state = next;
    record.owner = Some(owner.to_string());
    record.updated_at_ms = at_ms;
    Ok(bad_case_artifact(record))
}

pub fn promote_bad_case_to_fixture(
    artifact: &BadCaseArtifactV1,
    fixture_id: &str,
    dataset_version: &str,
    owner: &str,
    at_ms: u64,
) -> Result<(BadCaseArtifactV1, RegressionFixtureArtifactV1), String> {
    validate_bad_case_artifact(artifact)?;
    if artifact.record.state != BadCaseState::Verified {
        return Err("only a verified bad case can be promoted".to_string());
    }
    if artifact.record.replay.expected_assertions.is_empty() {
        return Err("promotion requires at least one deterministic assertion".to_string());
    }
    validate_artifact_id(fixture_id, "fixture_id")?;
    validate_artifact_id(dataset_version, "dataset_version")?;
    let promoted = transition_bad_case(artifact, BadCaseState::Promoted, owner, "promoted", at_ms)?;
    let fixture = RegressionFixtureV1 {
        schema_version: REGRESSION_FIXTURE_SCHEMA_VERSION.to_string(),
        fixture_id: fixture_id.to_string(),
        dataset_version: dataset_version.to_string(),
        promoted_at_ms: at_ms,
        source_bad_case_id: artifact.record.bad_case_id.clone(),
        source_bad_case_fingerprint: artifact.content_fingerprint.clone(),
        source: artifact.record.source.clone(),
        replay: artifact.record.replay.clone(),
    };
    let fingerprint = fingerprint(&fixture)?;
    Ok((
        promoted,
        RegressionFixtureArtifactV1 {
            schema_version: REGRESSION_FIXTURE_ARTIFACT_SCHEMA_VERSION.to_string(),
            content_fingerprint: fingerprint,
            fixture,
        },
    ))
}

pub fn validate_bad_case_artifact(artifact: &BadCaseArtifactV1) -> Result<(), String> {
    if artifact.schema_version != BAD_CASE_ARTIFACT_SCHEMA_VERSION
        || artifact.record.schema_version != BAD_CASE_SCHEMA_VERSION
    {
        return Err("unsupported bad case artifact schema_version".to_string());
    }
    validate_artifact_id(&artifact.record.bad_case_id, "bad_case_id")?;
    validate_source(&artifact.record.source)?;
    if artifact.record.updated_at_ms < artifact.record.captured_at_ms {
        return Err("bad case updated_at_ms precedes captured_at_ms".to_string());
    }
    if !artifact.record.privacy_scan_passed
        || contains_sensitive_material(
            &serde_json::to_value(&artifact.record.replay)
                .map_err(|error| format!("serialize replay: {error}"))?,
        )
    {
        return Err("bad case replay did not pass privacy validation".to_string());
    }
    let expected = fingerprint(&artifact.record)?;
    if artifact.content_fingerprint != expected {
        return Err("bad case content fingerprint mismatch".to_string());
    }
    Ok(())
}

pub fn validate_regression_fixture_artifact(
    artifact: &RegressionFixtureArtifactV1,
) -> Result<(), String> {
    if artifact.schema_version != REGRESSION_FIXTURE_ARTIFACT_SCHEMA_VERSION
        || artifact.fixture.schema_version != REGRESSION_FIXTURE_SCHEMA_VERSION
    {
        return Err("unsupported regression fixture schema_version".to_string());
    }
    if artifact.fixture.replay.expected_assertions.is_empty() {
        return Err("regression fixture has no assertions".to_string());
    }
    validate_source(&artifact.fixture.source)?;
    let expected = fingerprint(&artifact.fixture)?;
    if artifact.content_fingerprint != expected {
        return Err("regression fixture content fingerprint mismatch".to_string());
    }
    if contains_sensitive_material(
        &serde_json::to_value(&artifact.fixture.replay)
            .map_err(|error| format!("serialize fixture replay: {error}"))?,
    ) {
        return Err("regression fixture contains sensitive material".to_string());
    }
    Ok(())
}

fn bad_case_artifact(record: BadCaseRecordV1) -> BadCaseArtifactV1 {
    let content_fingerprint =
        fingerprint(&record).unwrap_or_else(|_| canonical_json_fingerprint(&Value::Null));
    BadCaseArtifactV1 {
        schema_version: BAD_CASE_ARTIFACT_SCHEMA_VERSION.to_string(),
        content_fingerprint,
        record,
    }
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_value(value)
        .map(|value| canonical_json_fingerprint(&value))
        .map_err(|error| format!("serialize fingerprint source: {error}"))
}

fn validate_source(source: &BadCaseSourceV1) -> Result<(), String> {
    validate_identity(&source.session_id, "session_id")?;
    validate_identity(&source.run_id, "run_id")?;
    if !valid_hex(&source.trace_id, 32) {
        return Err("trace_id must be 32 hexadecimal characters".to_string());
    }
    if !valid_hex(&source.task_contract_fingerprint, 64)
        || !valid_hex(&source.versions.config_fingerprint, 64)
    {
        return Err(
            "task contract and config fingerprints must be 64 hexadecimal characters".to_string(),
        );
    }
    for (name, value) in [
        ("harness_version", source.versions.harness_version.as_str()),
        ("agent_version", source.versions.agent_version.as_str()),
        ("prompt_version", source.versions.prompt_version.as_str()),
        (
            "skill_set_version",
            source.versions.skill_set_version.as_str(),
        ),
        (
            "tool_set_version",
            source.versions.tool_set_version.as_str(),
        ),
    ] {
        validate_identity(value, name)?;
    }
    Ok(())
}

fn validate_identity(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{name} must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_artifact_id(value: &str, name: &str) -> Result<(), String> {
    validate_identity(value, name)?;
    if value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(format!(
            "{name} may contain only ASCII letters, numbers, dot, colon, dash, and underscore"
        ));
    }
    Ok(())
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && !value.bytes().all(|byte| byte == b'0')
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sanitize_value(value: &Value, path: &str, redacted_paths: &mut Vec<String>) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let child_path = format!("{path}.{key}");
                    if sensitive_key(key) {
                        redacted_paths.push(child_path);
                        (key.clone(), Value::String("[REDACTED]".to_string()))
                    } else {
                        (
                            key.clone(),
                            sanitize_value(value, &child_path, redacted_paths),
                        )
                    }
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    sanitize_value(value, &format!("{path}[{index}]"), redacted_paths)
                })
                .collect(),
        ),
        Value::String(raw) => {
            let sanitized = sanitize_string(raw);
            if sanitized != *raw {
                redacted_paths.push(path.to_string());
            }
            Value::String(sanitized)
        }
        _ => value.clone(),
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "authorization",
        "cookie",
        "credential",
        "password",
        "private_key",
        "refresh_token",
        "secret",
        "session_token",
        "token",
    ]
    .iter()
    .any(|candidate| key == *candidate || key.ends_with(&format!("_{candidate}")))
}

fn sanitize_string(raw: &str) -> String {
    let patterns = [
        r"(?i)bearer\s+[a-z0-9._~+\-/]+=*",
        r"\bsk-[A-Za-z0-9_-]{8,}\b",
        r"\bAKIA[0-9A-Z]{16}\b",
    ];
    patterns.iter().fold(raw.to_string(), |value, pattern| {
        Regex::new(pattern)
            .map(|regex| regex.replace_all(&value, "[REDACTED]").into_owned())
            .unwrap_or(value)
    })
}

fn contains_sensitive_material(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (sensitive_key(key) && value.as_str() != Some("[REDACTED]"))
                || contains_sensitive_material(value)
        }),
        Value::Array(items) => items.iter().any(contains_sensitive_material),
        Value::String(raw) => sanitize_string(raw) != *raw,
        _ => false,
    }
}

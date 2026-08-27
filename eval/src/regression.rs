use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    time::{Instant, SystemTime},
};

use openagent_telemetry::canonical_json_fingerprint;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    BadCaseAssertionV1, RegressionFixtureArtifactV1, RegressionFixtureV1,
    validate_regression_fixture_artifact,
};

pub const REGRESSION_DATASET_SCHEMA_VERSION: &str = "openharness.regression_dataset.v1";
pub const REGRESSION_OBSERVATIONS_SCHEMA_VERSION: &str = "openharness.regression_observations.v1";
pub const REGRESSION_REPLAY_REPORT_SCHEMA_VERSION: &str = "openharness.regression_replay.report.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionDatasetEntryV1 {
    pub fixture_id: String,
    pub path: String,
    pub content_fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionDatasetManifestV1 {
    pub schema_version: String,
    pub dataset_id: String,
    pub dataset_version: String,
    pub dataset_fingerprint: String,
    pub fixtures: Vec<RegressionDatasetEntryV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionObservationSetV1 {
    pub schema_version: String,
    pub observations: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegressionReplayCaseStatus {
    Passed,
    Failed,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionAssertionResultV1 {
    pub kind: String,
    pub path: String,
    pub operator: String,
    pub expected: Value,
    pub actual_fingerprint: Option<String>,
    pub passed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionReplayCaseResultV1 {
    pub fixture_id: String,
    pub fixture_fingerprint: String,
    pub status: RegressionReplayCaseStatus,
    pub duration_ms: u64,
    pub response_fingerprint: Option<String>,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub trace_id: Option<String>,
    pub assertions: Vec<RegressionAssertionResultV1>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionReplayReportV1 {
    pub schema_version: String,
    pub generated_at_ms: u64,
    pub dataset_id: String,
    pub dataset_version: String,
    pub dataset_fingerprint: String,
    pub executor_kind: String,
    pub total_cases: u64,
    pub passed_cases: u64,
    pub failed_cases: u64,
    pub error_cases: u64,
    pub passed: bool,
    pub cases: Vec<RegressionReplayCaseResultV1>,
}

pub fn build_regression_dataset_manifest(
    dataset_id: &str,
    dataset_version: &str,
    manifest_dir: &Path,
    fixture_paths: &[PathBuf],
) -> Result<RegressionDatasetManifestV1, String> {
    validate_id(dataset_id, "dataset_id")?;
    validate_id(dataset_version, "dataset_version")?;
    if fixture_paths.is_empty() {
        return Err("regression dataset must contain at least one fixture".to_string());
    }
    let manifest_dir = fs::canonicalize(manifest_dir)
        .map_err(|error| format!("resolve manifest directory: {error}"))?;
    let mut fixtures = Vec::new();
    for fixture_path in fixture_paths {
        let canonical = fs::canonicalize(fixture_path)
            .map_err(|error| format!("resolve {}: {error}", fixture_path.display()))?;
        let relative = canonical.strip_prefix(&manifest_dir).map_err(|_| {
            format!(
                "fixture {} must be inside manifest directory {}",
                canonical.display(),
                manifest_dir.display()
            )
        })?;
        validate_relative_path(relative)?;
        let artifact: RegressionFixtureArtifactV1 = read_json(&canonical)?;
        validate_regression_fixture_artifact(&artifact)?;
        if artifact.fixture.dataset_version != dataset_version {
            return Err(format!(
                "fixture {} dataset_version {} does not match {}",
                artifact.fixture.fixture_id, artifact.fixture.dataset_version, dataset_version
            ));
        }
        fixtures.push(RegressionDatasetEntryV1 {
            fixture_id: artifact.fixture.fixture_id,
            path: relative.to_string_lossy().replace('\\', "/"),
            content_fingerprint: artifact.content_fingerprint,
        });
    }
    fixtures.sort_by(|left, right| left.fixture_id.cmp(&right.fixture_id));
    let mut manifest = RegressionDatasetManifestV1 {
        schema_version: REGRESSION_DATASET_SCHEMA_VERSION.to_string(),
        dataset_id: dataset_id.to_string(),
        dataset_version: dataset_version.to_string(),
        dataset_fingerprint: String::new(),
        fixtures,
    };
    manifest.dataset_fingerprint = regression_dataset_fingerprint(&manifest);
    validate_regression_dataset_manifest(&manifest, &manifest_dir)?;
    Ok(manifest)
}

pub fn validate_regression_dataset_manifest(
    manifest: &RegressionDatasetManifestV1,
    manifest_dir: &Path,
) -> Result<Vec<RegressionFixtureArtifactV1>, String> {
    if manifest.schema_version != REGRESSION_DATASET_SCHEMA_VERSION {
        return Err(format!(
            "unsupported regression dataset schema_version: {}",
            manifest.schema_version
        ));
    }
    validate_id(&manifest.dataset_id, "dataset_id")?;
    validate_id(&manifest.dataset_version, "dataset_version")?;
    if manifest.fixtures.is_empty() {
        return Err("regression dataset must contain at least one fixture".to_string());
    }
    let expected_fingerprint = regression_dataset_fingerprint(manifest);
    if manifest.dataset_fingerprint != expected_fingerprint {
        return Err("regression dataset fingerprint mismatch".to_string());
    }

    let base = fs::canonicalize(manifest_dir)
        .map_err(|error| format!("resolve manifest directory: {error}"))?;
    let mut fixture_ids = BTreeSet::new();
    let mut source_ids = BTreeSet::new();
    let mut previous_id = None::<&str>;
    let mut artifacts = Vec::new();
    for entry in &manifest.fixtures {
        validate_id(&entry.fixture_id, "fixture_id")?;
        if previous_id.is_some_and(|previous| previous >= entry.fixture_id.as_str()) {
            return Err(
                "regression dataset fixtures must be uniquely sorted by fixture_id".to_string(),
            );
        }
        previous_id = Some(entry.fixture_id.as_str());
        if !fixture_ids.insert(entry.fixture_id.clone()) {
            return Err(format!("duplicate fixture_id: {}", entry.fixture_id));
        }
        let relative = Path::new(&entry.path);
        validate_relative_path(relative)?;
        let resolved = fs::canonicalize(base.join(relative))
            .map_err(|error| format!("resolve fixture {}: {error}", entry.path))?;
        if !resolved.starts_with(&base) {
            return Err(format!(
                "fixture path escapes manifest directory: {}",
                entry.path
            ));
        }
        let artifact: RegressionFixtureArtifactV1 = read_json(&resolved)?;
        validate_regression_fixture_artifact(&artifact)?;
        if artifact.fixture.fixture_id != entry.fixture_id
            || artifact.fixture.dataset_version != manifest.dataset_version
            || artifact.content_fingerprint != entry.content_fingerprint
        {
            return Err(format!(
                "fixture entry does not match artifact: {}",
                entry.fixture_id
            ));
        }
        if !source_ids.insert(artifact.fixture.source_bad_case_id.clone()) {
            return Err(format!(
                "bad case {} is promoted more than once in dataset",
                artifact.fixture.source_bad_case_id
            ));
        }
        artifacts.push(artifact);
    }
    Ok(artifacts)
}

#[must_use]
pub fn regression_dataset_fingerprint(manifest: &RegressionDatasetManifestV1) -> String {
    canonical_json_fingerprint(&json!({
        "schema_version": manifest.schema_version,
        "dataset_id": manifest.dataset_id,
        "dataset_version": manifest.dataset_version,
        "fixtures": manifest.fixtures,
    }))
}

pub fn validate_regression_observations(
    observations: &RegressionObservationSetV1,
    fixtures: &[RegressionFixtureArtifactV1],
) -> Result<(), String> {
    if observations.schema_version != REGRESSION_OBSERVATIONS_SCHEMA_VERSION {
        return Err(format!(
            "unsupported regression observations schema_version: {}",
            observations.schema_version
        ));
    }
    let expected = fixtures
        .iter()
        .map(|fixture| fixture.fixture.fixture_id.clone())
        .collect::<BTreeSet<_>>();
    let actual = observations
        .observations
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if expected != actual {
        return Err(format!(
            "observation fixture set mismatch: missing={:?}, unexpected={:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

pub fn replay_regression_dataset<F>(
    manifest: &RegressionDatasetManifestV1,
    manifest_dir: &Path,
    executor_kind: &str,
    mut executor: F,
) -> Result<RegressionReplayReportV1, String>
where
    F: FnMut(&RegressionFixtureV1) -> Result<Value, String>,
{
    validate_id(executor_kind, "executor_kind")?;
    let fixtures = validate_regression_dataset_manifest(manifest, manifest_dir)?;
    let mut cases = Vec::new();
    for artifact in fixtures {
        let started = Instant::now();
        let fixture = &artifact.fixture;
        let result = match executor(fixture) {
            Ok(observation) => {
                let assertions = fixture
                    .replay
                    .expected_assertions
                    .iter()
                    .map(|assertion| evaluate_assertion(assertion, &observation))
                    .collect::<Vec<_>>();
                let passed = assertions.iter().all(|assertion| assertion.passed);
                RegressionReplayCaseResultV1 {
                    fixture_id: fixture.fixture_id.clone(),
                    fixture_fingerprint: artifact.content_fingerprint.clone(),
                    status: if passed {
                        RegressionReplayCaseStatus::Passed
                    } else {
                        RegressionReplayCaseStatus::Failed
                    },
                    duration_ms: elapsed_ms(started),
                    response_fingerprint: Some(canonical_json_fingerprint(&observation)),
                    session_id: correlation_id(&observation, &["session_id"]),
                    run_id: correlation_id(&observation, &["run_id", "turn_id"]),
                    trace_id: correlation_id(&observation, &["trace_id"]),
                    assertions,
                    error: None,
                }
            }
            Err(error) => RegressionReplayCaseResultV1 {
                fixture_id: fixture.fixture_id.clone(),
                fixture_fingerprint: artifact.content_fingerprint.clone(),
                status: RegressionReplayCaseStatus::Error,
                duration_ms: elapsed_ms(started),
                response_fingerprint: None,
                session_id: None,
                run_id: None,
                trace_id: None,
                assertions: Vec::new(),
                error: Some(error),
            },
        };
        cases.push(result);
    }
    let passed_cases = count_status(&cases, RegressionReplayCaseStatus::Passed);
    let failed_cases = count_status(&cases, RegressionReplayCaseStatus::Failed);
    let error_cases = count_status(&cases, RegressionReplayCaseStatus::Error);
    let total_cases = u64::try_from(cases.len()).unwrap_or(u64::MAX);
    Ok(RegressionReplayReportV1 {
        schema_version: REGRESSION_REPLAY_REPORT_SCHEMA_VERSION.to_string(),
        generated_at_ms: now_ms(),
        dataset_id: manifest.dataset_id.clone(),
        dataset_version: manifest.dataset_version.clone(),
        dataset_fingerprint: manifest.dataset_fingerprint.clone(),
        executor_kind: executor_kind.to_string(),
        total_cases,
        passed_cases,
        failed_cases,
        error_cases,
        passed: total_cases > 0 && passed_cases == total_cases,
        cases,
    })
}

fn evaluate_assertion(
    assertion: &BadCaseAssertionV1,
    observation: &Value,
) -> RegressionAssertionResultV1 {
    let actual = json_path(observation, &assertion.path);
    let evaluation = if assertion.kind != "json_path" {
        Err(format!("unsupported assertion kind: {}", assertion.kind))
    } else {
        evaluate_operator(assertion, actual)
    };
    RegressionAssertionResultV1 {
        kind: assertion.kind.clone(),
        path: assertion.path.clone(),
        operator: assertion.operator.clone(),
        expected: assertion.expected.clone(),
        actual_fingerprint: actual.map(canonical_json_fingerprint),
        passed: evaluation.as_ref().is_ok_and(|passed| *passed),
        error: match evaluation {
            Ok(true) => None,
            Ok(false) => Some("assertion did not match".to_string()),
            Err(error) => Some(error),
        },
    }
}

fn evaluate_operator(
    assertion: &BadCaseAssertionV1,
    actual: Option<&Value>,
) -> Result<bool, String> {
    match assertion.operator.as_str() {
        "exists" => Ok(actual.is_some()),
        "not_exists" => Ok(actual.is_none()),
        "eq" => Ok(actual == Some(&assertion.expected)),
        "ne" => Ok(actual.is_some_and(|actual| actual != &assertion.expected)),
        "contains" => Ok(actual.is_some_and(|actual| match actual {
            Value::String(value) => assertion
                .expected
                .as_str()
                .is_some_and(|expected| value.contains(expected)),
            Value::Array(values) => values.contains(&assertion.expected),
            Value::Object(values) => assertion
                .expected
                .as_str()
                .is_some_and(|expected| values.contains_key(expected)),
            _ => false,
        })),
        "gte" | "lte" => {
            let actual = actual
                .and_then(Value::as_f64)
                .ok_or_else(|| "numeric comparison requires an observed number".to_string())?;
            let expected = assertion.expected.as_f64().ok_or_else(|| {
                "numeric comparison requires a numeric expected value".to_string()
            })?;
            Ok(if assertion.operator == "gte" {
                actual >= expected
            } else {
                actual <= expected
            })
        }
        "regex" => {
            let actual = actual
                .and_then(Value::as_str)
                .ok_or_else(|| "regex comparison requires an observed string".to_string())?;
            let pattern = assertion
                .expected
                .as_str()
                .ok_or_else(|| "regex comparison requires a string pattern".to_string())?;
            Regex::new(pattern)
                .map(|regex| regex.is_match(actual))
                .map_err(|error| format!("invalid assertion regex: {error}"))
        }
        operator => Err(format!("unsupported assertion operator: {operator}")),
    }
}

fn json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    let mut remaining = path.strip_prefix('$')?;
    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix('.') {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            if end == 0 {
                return None;
            }
            current = current.get(&rest[..end])?;
            remaining = &rest[end..];
        } else {
            let rest = remaining.strip_prefix('[')?;
            let end = rest.find(']')?;
            let index = rest[..end].parse::<usize>().ok()?;
            current = current.get(index)?;
            remaining = &rest[end + 1..];
        }
    }
    Some(current)
}

fn correlation_id(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = value.get(*key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
    }
    match value {
        Value::Object(object) => object
            .values()
            .find_map(|value| correlation_id(value, keys)),
        Value::Array(items) => items.iter().find_map(|value| correlation_id(value, keys)),
        _ => None,
    }
}

fn validate_relative_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "fixture path must be a normalized relative path: {}",
            path.display()
        ));
    }
    Ok(())
}

fn validate_id(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(format!("invalid {name}"));
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn count_status(cases: &[RegressionReplayCaseResultV1], status: RegressionReplayCaseStatus) -> u64 {
    u64::try_from(cases.iter().filter(|case| case.status == status).count()).unwrap_or(u64::MAX)
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::json_path;

    #[test]
    fn json_path_supports_fields_and_array_indices() {
        let value = json!({"events": [{"status": "completed"}]});
        assert_eq!(
            json_path(&value, "$.events[0].status"),
            Some(&json!("completed"))
        );
        assert!(json_path(&value, "events[0]").is_none());
    }
}

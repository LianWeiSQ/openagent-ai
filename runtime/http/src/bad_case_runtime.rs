use super::*;
use openagent_eval::{
    BAD_CASE_CAPTURE_SCHEMA_VERSION, BadCaseArtifactV1, BadCaseAssertionV1, BadCaseCaptureInputV1,
    BadCaseCategory, BadCaseReplayV1, BadCaseSeverity, BadCaseState, capture_bad_case,
    promote_bad_case_to_fixture, transition_bad_case,
};

fn bad_case_root(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config)
        .join(".openagent-runtime")
        .join("bad-cases")
}

fn regression_fixture_root(config: &HttpRuntimeConfig, dataset_version: &str) -> PathBuf {
    session_root(config)
        .join(".openagent-runtime")
        .join("regression-fixtures")
        .join(dataset_version)
}

fn bad_case_path(config: &HttpRuntimeConfig, bad_case_id: &str) -> PathBuf {
    bad_case_root(config).join(format!("{bad_case_id}.json"))
}

pub(super) fn capture_turn_bad_case_payload(
    config: &HttpRuntimeConfig,
    turn_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let store = FileSessionStore::new(session_root(config));
    let (session_id, session) = find_session_for_turn(&store, turn_id)?;
    let run_path = store
        .root
        .join(&session_id)
        .join("runs")
        .join(turn_id)
        .join("run.json");
    let run = read_json_file(&run_path);
    if run.is_null() {
        return Err(format!("run record not found: {turn_id}"));
    }
    let source = openagent_eval::bad_case_source_from_run(&run)?;
    let category = parse_enum_field::<BadCaseCategory>(&payload, "category", json!("other"))?;
    let severity = parse_enum_field::<BadCaseSeverity>(&payload, "severity", json!("medium"))?;
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "bad case title is required".to_string())?;
    let replay_input = payload.get("replay_input").cloned().unwrap_or_else(|| {
        json!({
            "input": latest_user_message(&session),
        })
    });
    let expected_assertions = serde_json::from_value::<Vec<BadCaseAssertionV1>>(
        payload
            .get("expected_assertions")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .map_err(|error| format!("invalid expected_assertions: {error}"))?;
    let tags = payload
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let bad_case_id = payload
        .get("bad_case_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| new_id("bad_case"));
    let artifact = capture_bad_case(BadCaseCaptureInputV1 {
        schema_version: BAD_CASE_CAPTURE_SCHEMA_VERSION.to_string(),
        bad_case_id: bad_case_id.clone(),
        captured_at_ms: now_ms(),
        category,
        severity,
        title: title.to_string(),
        owner: payload
            .get("owner")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        source,
        replay: BadCaseReplayV1 {
            input: replay_input,
            runtime_overrides: payload
                .get("runtime_overrides")
                .cloned()
                .unwrap_or_else(|| json!({})),
            expected_assertions,
            tags,
        },
    })?;
    persist_bad_case(config, &artifact)?;
    record_bad_case_event(&store, &artifact, "bad_case.captured", "captured");
    serde_json::to_value(artifact).map_err(|error| error.to_string())
}

pub(super) fn list_bad_cases_payload(config: &HttpRuntimeConfig) -> Value {
    let root = bad_case_root(config);
    let mut artifacts = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .filter_map(|path| fs::read_to_string(path).ok())
        .filter_map(|raw| serde_json::from_str::<BadCaseArtifactV1>(&raw).ok())
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| {
        right
            .record
            .updated_at_ms
            .cmp(&left.record.updated_at_ms)
            .then_with(|| left.record.bad_case_id.cmp(&right.record.bad_case_id))
    });
    json!({
        "schema_version": "openharness.bad_case.list.v1",
        "count": artifacts.len(),
        "bad_cases": artifacts,
    })
}

pub(super) fn get_bad_case_payload(
    config: &HttpRuntimeConfig,
    bad_case_id: &str,
) -> Result<Value, String> {
    let artifact = load_bad_case(config, bad_case_id)?;
    serde_json::to_value(artifact).map_err(|error| error.to_string())
}

pub(super) fn transition_bad_case_payload(
    config: &HttpRuntimeConfig,
    bad_case_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let next = parse_enum_field::<BadCaseState>(&payload, "state", Value::Null)?;
    let owner = payload
        .get("owner")
        .and_then(Value::as_str)
        .ok_or_else(|| "bad case transition owner is required".to_string())?;
    let note = payload
        .get("note")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let artifact = load_bad_case(config, bad_case_id)?;
    let updated = transition_bad_case(&artifact, next, owner, note, now_ms())?;
    persist_bad_case(config, &updated)?;
    let store = FileSessionStore::new(session_root(config));
    record_bad_case_event(&store, &updated, "bad_case.transitioned", "updated");
    serde_json::to_value(updated).map_err(|error| error.to_string())
}

pub(super) fn promote_bad_case_payload(
    config: &HttpRuntimeConfig,
    bad_case_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let required = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{key} is required"))
    };
    let fixture_id = required("fixture_id")?;
    let dataset_version = required("dataset_version")?;
    let owner = required("owner")?;
    let artifact = load_bad_case(config, bad_case_id)?;
    let (promoted, fixture) =
        promote_bad_case_to_fixture(&artifact, fixture_id, dataset_version, owner, now_ms())?;
    persist_bad_case(config, &promoted)?;
    let fixture_root = regression_fixture_root(config, dataset_version);
    fs::create_dir_all(&fixture_root).map_err(|error| error.to_string())?;
    write_json_value(
        &fixture_root.join(format!("{fixture_id}.json")),
        &serde_json::to_value(&fixture).map_err(|error| error.to_string())?,
    )?;
    let store = FileSessionStore::new(session_root(config));
    record_bad_case_event(&store, &promoted, "bad_case.promoted", "promoted");
    Ok(json!({
        "bad_case": promoted,
        "fixture": fixture,
        "fixture_path": fixture_root.join(format!("{fixture_id}.json")),
    }))
}

fn persist_bad_case(
    config: &HttpRuntimeConfig,
    artifact: &BadCaseArtifactV1,
) -> Result<(), String> {
    let root = bad_case_root(config);
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    write_json_value(
        &bad_case_path(config, &artifact.record.bad_case_id),
        &serde_json::to_value(artifact).map_err(|error| error.to_string())?,
    )
}

fn load_bad_case(
    config: &HttpRuntimeConfig,
    bad_case_id: &str,
) -> Result<BadCaseArtifactV1, String> {
    if !bad_case_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err("invalid bad_case_id".to_string());
    }
    let raw = fs::read_to_string(bad_case_path(config, bad_case_id))
        .map_err(|_| format!("bad case not found: {bad_case_id}"))?;
    serde_json::from_str(&raw).map_err(|error| format!("invalid bad case artifact: {error}"))
}

fn record_bad_case_event(
    store: &FileSessionStore,
    artifact: &BadCaseArtifactV1,
    event: &str,
    status: &str,
) {
    let _ = store.record_event(
        &artifact.record.source.session_id,
        &artifact.record.source.run_id,
        event,
        SessionEventOptions {
            kind: "bad_case".to_string(),
            status: status.to_string(),
            trace_id: Some(artifact.record.source.trace_id.clone()),
            attributes: BTreeMap::from([
                (
                    "bad_case_id".to_string(),
                    json!(artifact.record.bad_case_id),
                ),
                ("state".to_string(), json!(artifact.record.state)),
                ("severity".to_string(), json!(artifact.record.severity)),
                (
                    "content_fingerprint".to_string(),
                    json!(artifact.content_fingerprint),
                ),
            ]),
            ..SessionEventOptions::default()
        },
    );
}

fn parse_enum_field<T: serde::de::DeserializeOwned>(
    payload: &Value,
    key: &str,
    default: Value,
) -> Result<T, String> {
    serde_json::from_value(payload.get(key).cloned().unwrap_or(default))
        .map_err(|error| format!("invalid {key}: {error}"))
}

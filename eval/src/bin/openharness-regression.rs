use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use openagent_eval::{
    RegressionDatasetManifestV1, RegressionFixtureV1, RegressionObservationSetV1,
    build_regression_dataset_manifest, replay_regression_dataset,
    validate_regression_dataset_manifest, validate_regression_observations,
};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{Map, Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            eprintln!("openharness-regression: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<bool, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("index") if args.len() >= 5 => {
            let output = Path::new(&args[3]);
            let manifest_dir = output.parent().unwrap_or_else(|| Path::new("."));
            fs::create_dir_all(manifest_dir)
                .map_err(|error| format!("create {}: {error}", manifest_dir.display()))?;
            let fixture_paths = args[4..].iter().map(PathBuf::from).collect::<Vec<_>>();
            let manifest = build_regression_dataset_manifest(
                &args[1],
                &args[2],
                manifest_dir,
                &fixture_paths,
            )?;
            write_json(output, &manifest)?;
            println!(
                "regression dataset: fixtures={}; fingerprint={}; output={}",
                manifest.fixtures.len(),
                manifest.dataset_fingerprint,
                output.display()
            );
            Ok(true)
        }
        Some("validate") if args.len() == 2 => {
            let manifest_path = Path::new(&args[1]);
            let manifest: RegressionDatasetManifestV1 = read_json(manifest_path)?;
            let fixtures = validate_regression_dataset_manifest(
                &manifest,
                manifest_path.parent().unwrap_or_else(|| Path::new(".")),
            )?;
            println!(
                "regression dataset valid: id={}; version={}; fixtures={}; fingerprint={}",
                manifest.dataset_id,
                manifest.dataset_version,
                fixtures.len(),
                manifest.dataset_fingerprint
            );
            Ok(true)
        }
        Some("replay-observations") if args.len() == 4 => {
            let manifest_path = Path::new(&args[1]);
            let manifest: RegressionDatasetManifestV1 = read_json(manifest_path)?;
            let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
            let fixtures = validate_regression_dataset_manifest(&manifest, base)?;
            let observations: RegressionObservationSetV1 = read_json(Path::new(&args[2]))?;
            validate_regression_observations(&observations, &fixtures)?;
            let report = replay_regression_dataset(
                &manifest,
                base,
                "recorded_observations",
                |fixture| {
                    observations
                        .observations
                        .get(&fixture.fixture_id)
                        .cloned()
                        .ok_or_else(|| format!("missing observation: {}", fixture.fixture_id))
                },
            )?;
            write_json(Path::new(&args[3]), &report)?;
            println!(
                "regression replay: passed={}; cases={}; output={}",
                report.passed, report.total_cases, args[3]
            );
            Ok(report.passed)
        }
        Some("replay-bridge") if args.len() == 5 || args.len() == 6 => {
            let manifest_path = Path::new(&args[1]);
            let manifest: RegressionDatasetManifestV1 = read_json(manifest_path)?;
            let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
            let bridge = BridgeReplayClient::new(
                &args[2],
                &args[3],
                args.get(5).map(String::as_str),
            )?;
            let report = replay_regression_dataset(
                &manifest,
                base,
                "bridge_http",
                |fixture| bridge.execute(fixture),
            )?;
            write_json(Path::new(&args[4]), &report)?;
            println!(
                "regression replay: passed={}; cases={}; output={}",
                report.passed, report.total_cases, args[4]
            );
            Ok(report.passed)
        }
        _ => Err(
            "usage: openharness-regression index <dataset-id> <dataset-version> <manifest.json> <fixture.json>... | validate <manifest.json> | replay-observations <manifest.json> <observations.json> <report.json> | replay-bridge <manifest.json> <bridge-url> <workspace> <report.json> [token-env]"
                .to_string(),
        ),
    }
}

struct BridgeReplayClient {
    client: Client,
    base_url: String,
    workspace: String,
    token: Option<String>,
}

impl BridgeReplayClient {
    fn new(base_url: &str, workspace: &str, token_env: Option<&str>) -> Result<Self, String> {
        let workspace = fs::canonicalize(workspace)
            .map_err(|error| format!("resolve replay workspace {workspace}: {error}"))?;
        let token = token_env
            .map(|name| {
                env::var(name)
                    .map_err(|_| format!("bridge token environment variable is unset: {name}"))
            })
            .transpose()?;
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .no_proxy()
            .build()
            .map_err(|error| format!("build Bridge replay client: {error}"))?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            workspace: workspace.to_string_lossy().to_string(),
            token,
        })
    }

    fn execute(&self, fixture: &RegressionFixtureV1) -> Result<Value, String> {
        let session = self.send_json(
            self.client
                .post(format!("{}/api/sessions", self.base_url))
                .json(&json!({"cwd": self.workspace})),
        )?;
        let session_id = session
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "Bridge create-session response is missing session_id".to_string())?;
        let payload = replay_payload(fixture)?;
        self.send_json(
            self.client
                .post(format!(
                    "{}/api/sessions/{}/turns",
                    self.base_url, session_id
                ))
                .json(&payload),
        )
    }

    fn send_json(&self, request: RequestBuilder) -> Result<Value, String> {
        let request = if let Some(token) = &self.token {
            request.bearer_auth(token)
        } else {
            request
        };
        let response = request
            .send()
            .map_err(|error| format!("Bridge replay request failed: {error}"))?;
        let status = response.status();
        let trace_id = response
            .headers()
            .get("traceparent")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split('-').nth(1))
            .map(ToString::to_string);
        if !status.is_success() {
            return Err(format!("Bridge replay returned HTTP {status}"));
        }
        let mut value = response
            .json::<Value>()
            .map_err(|error| format!("parse Bridge replay response: {error}"))?;
        if let (Some(trace_id), Some(object)) = (trace_id, value.as_object_mut()) {
            object
                .entry("trace_id".to_string())
                .or_insert(json!(trace_id));
        }
        Ok(value)
    }
}

fn replay_payload(fixture: &RegressionFixtureV1) -> Result<Value, String> {
    let mut payload = fixture.replay.input.as_object().cloned().ok_or_else(|| {
        format!(
            "fixture {} replay input must be an object",
            fixture.fixture_id
        )
    })?;
    match &fixture.replay.runtime_overrides {
        Value::Null => {}
        Value::Object(overrides) => merge_objects(&mut payload, overrides),
        _ => {
            return Err(format!(
                "fixture {} runtime_overrides must be an object",
                fixture.fixture_id
            ));
        }
    }
    payload.remove("async");
    Ok(Value::Object(payload))
}

fn merge_objects(target: &mut Map<String, Value>, overrides: &Map<String, Value>) {
    for (key, value) in overrides {
        match (target.get_mut(key), value) {
            (Some(Value::Object(target)), Value::Object(overrides)) => {
                merge_objects(target, overrides);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    fs::write(path, format!("{raw}\n"))
        .map_err(|error| format!("write {}: {error}", path.display()))
}

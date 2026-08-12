use std::{
    io::Read,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant, SystemTime},
};

use openagent_telemetry::canonical_json_fingerprint;
use reqwest::{Method, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const LOAD_TEST_PLAN_SCHEMA_VERSION: &str = "openharness.load_test.plan.v1";
pub const LOAD_TEST_REPORT_SCHEMA_VERSION: &str = "openharness.load_test.report.v1";
pub const LOAD_TEST_BASELINE_SCHEMA_VERSION: &str = "openharness.load_test.baseline.v1";

const MAX_CONCURRENCY: u64 = 64;
const MAX_REQUESTS_PER_WORKER: u64 = 100_000;
const MAX_DURATION_SECONDS: u64 = 86_400;
const MAX_FAILURE_SAMPLES: usize = 50;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadTestMode {
    Http,
    BridgeTurn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadTestPlanV1 {
    pub schema_version: String,
    pub workload_id: String,
    pub mode: LoadTestMode,
    pub base_url: String,
    pub workspace: Option<String>,
    pub auth_token_env: Option<String>,
    pub method: String,
    pub path: String,
    pub request_body: Value,
    pub concurrency: u64,
    pub requests_per_worker: u64,
    pub duration_seconds: Option<u64>,
    pub think_time_ms: u64,
    pub request_timeout_ms: u64,
    pub max_response_bytes: u64,
    pub use_system_proxy: bool,
    pub min_success_rate: f64,
    pub max_p95_duration_ms: u64,
    pub max_p99_duration_ms: u64,
    pub max_average_tokens_per_request: Option<f64>,
    pub max_average_cost_microunits_per_request: Option<f64>,
    pub max_p95_regression_ratio: f64,
    pub max_token_regression_ratio: f64,
    pub max_cost_regression_ratio: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadTestMetricsV1 {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub success_rate: f64,
    pub elapsed_ms: u64,
    pub requests_per_second: f64,
    pub p50_duration_ms: u64,
    pub p95_duration_ms: u64,
    pub p99_duration_ms: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
    pub total_cost_microunits: u64,
    pub average_tokens_per_request: f64,
    pub average_cost_microunits_per_request: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadFailureSampleV1 {
    pub worker_id: u64,
    pub request_index: u64,
    pub http_status: Option<u16>,
    pub duration_ms: u64,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub trace_id: Option<String>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadTestReportV1 {
    pub schema_version: String,
    pub workload_id: String,
    pub generated_at_ms: u64,
    pub plan_fingerprint: String,
    pub workload_fingerprint: String,
    pub baseline_id: Option<String>,
    pub baseline_fingerprint: Option<String>,
    pub metrics: LoadTestMetricsV1,
    pub p95_regression_ratio: Option<f64>,
    pub token_regression_ratio: Option<f64>,
    pub cost_regression_ratio: Option<f64>,
    pub failure_samples: Vec<LoadFailureSampleV1>,
    pub reasons: Vec<String>,
    pub passed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadTestBaselineV1 {
    pub schema_version: String,
    pub baseline_id: String,
    pub workload_id: String,
    pub workload_fingerprint: String,
    pub accepted_at_ms: u64,
    pub metrics: LoadTestMetricsV1,
    pub content_fingerprint: String,
}

#[derive(Clone, Debug)]
struct RequestObservation {
    worker_id: u64,
    request_index: u64,
    http_status: Option<u16>,
    duration_ms: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_microunits: u64,
    session_id: Option<String>,
    run_id: Option<String>,
    trace_id: Option<String>,
    error: Option<String>,
}

pub fn run_load_test(
    plan: &LoadTestPlanV1,
    baseline: Option<&LoadTestBaselineV1>,
) -> Result<LoadTestReportV1, String> {
    validate_load_test_plan(plan)?;
    if let Some(baseline) = baseline {
        validate_load_test_baseline(baseline)?;
        if baseline.workload_id != plan.workload_id
            || baseline.workload_fingerprint != load_test_workload_fingerprint(plan)
        {
            return Err("load baseline does not match workload identity".to_string());
        }
    }
    let token = plan
        .auth_token_env
        .as_deref()
        .map(|name| {
            std::env::var(name)
                .map_err(|_| format!("load-test auth token environment variable is unset: {name}"))
        })
        .transpose()?;
    let plan = Arc::new(plan.clone());
    let token = Arc::new(token);
    let barrier = Arc::new(Barrier::new(
        usize::try_from(plan.concurrency).map_err(|_| "invalid concurrency".to_string())?,
    ));
    let started = Instant::now();
    let deadline = plan
        .duration_seconds
        .map(|seconds| started + Duration::from_secs(seconds));
    let mut workers = Vec::new();
    for worker_id in 0..plan.concurrency {
        let plan = Arc::clone(&plan);
        let token = Arc::clone(&token);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            run_worker(worker_id, &plan, token.as_deref(), deadline, &barrier)
        }));
    }
    let mut observations = Vec::new();
    for worker in workers {
        observations.extend(
            worker
                .join()
                .map_err(|_| "load-test worker panicked".to_string())?,
        );
    }
    let elapsed_ms = elapsed_ms(started);
    Ok(build_load_test_report(
        &plan,
        baseline,
        observations,
        elapsed_ms,
    ))
}

pub fn load_test_baseline_from_report(
    report: &LoadTestReportV1,
    baseline_id: &str,
    accepted_at_ms: u64,
) -> Result<LoadTestBaselineV1, String> {
    validate_id(baseline_id, "baseline_id")?;
    if !report.passed {
        return Err("only a passing load report can become a baseline".to_string());
    }
    if report.metrics.total_requests == 0 || accepted_at_ms == 0 {
        return Err("baseline requires measured requests and accepted_at_ms".to_string());
    }
    let mut baseline = LoadTestBaselineV1 {
        schema_version: LOAD_TEST_BASELINE_SCHEMA_VERSION.to_string(),
        baseline_id: baseline_id.to_string(),
        workload_id: report.workload_id.clone(),
        workload_fingerprint: report.workload_fingerprint.clone(),
        accepted_at_ms,
        metrics: report.metrics.clone(),
        content_fingerprint: String::new(),
    };
    baseline.content_fingerprint = load_test_baseline_fingerprint(&baseline);
    Ok(baseline)
}

pub fn validate_load_test_plan(plan: &LoadTestPlanV1) -> Result<(), String> {
    if plan.schema_version != LOAD_TEST_PLAN_SCHEMA_VERSION {
        return Err(format!(
            "unsupported load-test schema_version: {}",
            plan.schema_version
        ));
    }
    validate_id(&plan.workload_id, "workload_id")?;
    if !plan.base_url.starts_with("http://") && !plan.base_url.starts_with("https://") {
        return Err("load-test base_url must use http or https".to_string());
    }
    if !plan.path.starts_with('/') || plan.path.starts_with("//") {
        return Err("load-test path must be an absolute HTTP path".to_string());
    }
    Method::from_bytes(plan.method.as_bytes())
        .map_err(|_| "load-test method is invalid".to_string())?;
    if plan.concurrency == 0 || plan.concurrency > MAX_CONCURRENCY {
        return Err(format!(
            "load-test concurrency must be 1..={MAX_CONCURRENCY}"
        ));
    }
    if plan.requests_per_worker > MAX_REQUESTS_PER_WORKER
        || (plan.requests_per_worker == 0 && plan.duration_seconds.is_none())
    {
        return Err(
            "load-test requires bounded requests_per_worker or duration_seconds".to_string(),
        );
    }
    if let Some(duration) = plan.duration_seconds
        && (duration == 0 || duration > MAX_DURATION_SECONDS)
    {
        return Err(format!(
            "load-test duration_seconds must be 1..={MAX_DURATION_SECONDS}"
        ));
    }
    if !(0.0..=1.0).contains(&plan.min_success_rate) {
        return Err("min_success_rate must be between zero and one".to_string());
    }
    for (name, value) in [
        ("max_p95_regression_ratio", plan.max_p95_regression_ratio),
        (
            "max_token_regression_ratio",
            plan.max_token_regression_ratio,
        ),
        ("max_cost_regression_ratio", plan.max_cost_regression_ratio),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{name} must be a finite non-negative number"));
        }
    }
    if plan.request_timeout_ms < 100 || plan.request_timeout_ms > 600_000 {
        return Err("request_timeout_ms must be between 100 and 600000".to_string());
    }
    if plan.max_response_bytes < 1024 || plan.max_response_bytes > 64 * 1024 * 1024 {
        return Err("max_response_bytes must be between 1024 and 67108864".to_string());
    }
    if plan.mode == LoadTestMode::BridgeTurn
        && plan
            .workspace
            .as_deref()
            .is_none_or(|workspace| workspace.trim().is_empty())
    {
        return Err("bridge_turn load-test mode requires workspace".to_string());
    }
    Ok(())
}

pub fn validate_load_test_baseline(baseline: &LoadTestBaselineV1) -> Result<(), String> {
    if baseline.schema_version != LOAD_TEST_BASELINE_SCHEMA_VERSION {
        return Err("unsupported load-test baseline schema_version".to_string());
    }
    validate_id(&baseline.baseline_id, "baseline_id")?;
    validate_id(&baseline.workload_id, "workload_id")?;
    if baseline.content_fingerprint != load_test_baseline_fingerprint(baseline) {
        return Err("load-test baseline fingerprint mismatch".to_string());
    }
    if baseline.metrics.total_requests == 0 {
        return Err("load-test baseline has no requests".to_string());
    }
    Ok(())
}

#[must_use]
pub fn load_test_plan_fingerprint(plan: &LoadTestPlanV1) -> String {
    serde_json::to_value(plan)
        .map(|value| canonical_json_fingerprint(&value))
        .unwrap_or_else(|_| canonical_json_fingerprint(&Value::Null))
}

#[must_use]
pub fn load_test_workload_fingerprint(plan: &LoadTestPlanV1) -> String {
    canonical_json_fingerprint(&json!({
        "workload_id": plan.workload_id,
        "mode": plan.mode,
        "method": plan.method,
        "path": plan.path,
        "request_body": plan.request_body,
        "concurrency": plan.concurrency,
        "think_time_ms": plan.think_time_ms,
    }))
}

fn run_worker(
    worker_id: u64,
    plan: &LoadTestPlanV1,
    token: Option<&str>,
    deadline: Option<Instant>,
    barrier: &Barrier,
) -> Vec<RequestObservation> {
    let mut client_builder =
        Client::builder().timeout(Duration::from_millis(plan.request_timeout_ms));
    if !plan.use_system_proxy {
        client_builder = client_builder.no_proxy();
    }
    let client = match client_builder.build() {
        Ok(client) => client,
        Err(error) => {
            barrier.wait();
            return vec![failed_observation(
                worker_id,
                0,
                0,
                format!("build HTTP client: {error}"),
            )];
        }
    };
    let session_id = if plan.mode == LoadTestMode::BridgeTurn {
        match create_bridge_session(&client, plan, token) {
            Ok(session_id) => Some(session_id),
            Err(error) => {
                barrier.wait();
                return vec![failed_observation(worker_id, 0, 0, error)];
            }
        }
    } else {
        None
    };
    barrier.wait();

    let mut observations = Vec::new();
    let mut request_index = 0_u64;
    loop {
        if plan.requests_per_worker > 0 && request_index >= plan.requests_per_worker {
            break;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        observations.push(execute_request(
            &client,
            plan,
            token,
            session_id.as_deref(),
            worker_id,
            request_index,
        ));
        request_index = request_index.saturating_add(1);
        if plan.think_time_ms > 0 {
            thread::sleep(Duration::from_millis(plan.think_time_ms));
        }
    }
    observations
}

fn create_bridge_session(
    client: &Client,
    plan: &LoadTestPlanV1,
    token: Option<&str>,
) -> Result<String, String> {
    let url = format!("{}/api/sessions", plan.base_url.trim_end_matches('/'));
    let workspace = plan.workspace.as_deref().unwrap_or_default();
    let mut request = client.post(url).json(&json!({"cwd": workspace}));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let (status, value) = bounded_json_response(request.send(), plan.max_response_bytes)?;
    if !(200..300).contains(&status) {
        return Err(format!("create Bridge session returned HTTP {status}"));
    }
    value
        .get("session_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| "create Bridge session response is missing session_id".to_string())
}

fn execute_request(
    client: &Client,
    plan: &LoadTestPlanV1,
    token: Option<&str>,
    session_id: Option<&str>,
    worker_id: u64,
    request_index: u64,
) -> RequestObservation {
    let started = Instant::now();
    let path = if let Some(session_id) = session_id {
        format!("/api/sessions/{session_id}/turns")
    } else {
        plan.path.clone()
    };
    let url = format!("{}{}", plan.base_url.trim_end_matches('/'), path);
    let method = Method::from_bytes(plan.method.as_bytes()).unwrap_or(Method::GET);
    let mut request = client.request(method, url);
    if !plan.request_body.is_null() {
        request = request.json(&plan.request_body);
    }
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    match bounded_json_response(request.send(), plan.max_response_bytes) {
        Ok((status, value)) => {
            let duration_ms = elapsed_ms(started);
            let runtime_status = value.get("status").and_then(Value::as_str);
            let response_failed = plan.mode == LoadTestMode::BridgeTurn
                && !matches!(runtime_status, Some("completed" | "degraded"));
            let error = if !(200..300).contains(&status) {
                Some(format!("HTTP {status}"))
            } else if response_failed {
                Some(format!(
                    "Bridge turn status {}",
                    runtime_status.unwrap_or("missing")
                ))
            } else {
                None
            };
            RequestObservation {
                worker_id,
                request_index,
                http_status: Some(status),
                duration_ms,
                input_tokens: recursive_u64(&value, &["input_tokens"]).unwrap_or(0),
                output_tokens: recursive_u64(&value, &["output_tokens"]).unwrap_or(0),
                cost_microunits: recursive_cost_microunits(&value),
                session_id: recursive_string(&value, &["session_id"]),
                run_id: recursive_string(&value, &["run_id", "turn_id"]),
                trace_id: recursive_string(&value, &["trace_id"]),
                error,
            }
        }
        Err(error) => failed_observation(worker_id, request_index, elapsed_ms(started), error),
    }
}

fn bounded_json_response(
    response: Result<reqwest::blocking::Response, reqwest::Error>,
    max_bytes: u64,
) -> Result<(u16, Value), String> {
    let response = response.map_err(|error| format!("HTTP request failed: {error}"))?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read HTTP response: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(format!("HTTP response exceeds {max_bytes} bytes"));
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse HTTP response JSON: {error}"))?;
    Ok((status, value))
}

fn failed_observation(
    worker_id: u64,
    request_index: u64,
    duration_ms: u64,
    error: String,
) -> RequestObservation {
    RequestObservation {
        worker_id,
        request_index,
        http_status: None,
        duration_ms,
        input_tokens: 0,
        output_tokens: 0,
        cost_microunits: 0,
        session_id: None,
        run_id: None,
        trace_id: None,
        error: Some(error),
    }
}

fn build_load_test_report(
    plan: &LoadTestPlanV1,
    baseline: Option<&LoadTestBaselineV1>,
    observations: Vec<RequestObservation>,
    elapsed_ms: u64,
) -> LoadTestReportV1 {
    let total_requests = u64::try_from(observations.len()).unwrap_or(u64::MAX);
    let successful_requests = u64::try_from(
        observations
            .iter()
            .filter(|observation| observation.error.is_none())
            .count(),
    )
    .unwrap_or(u64::MAX);
    let failed_requests = total_requests.saturating_sub(successful_requests);
    let success_rate = ratio(successful_requests, total_requests);
    let mut durations = observations
        .iter()
        .map(|observation| observation.duration_ms)
        .collect::<Vec<_>>();
    durations.sort_unstable();
    let total_input_tokens = observations
        .iter()
        .fold(0_u64, |total, item| total.saturating_add(item.input_tokens));
    let total_output_tokens = observations.iter().fold(0_u64, |total, item| {
        total.saturating_add(item.output_tokens)
    });
    let total_tokens = total_input_tokens.saturating_add(total_output_tokens);
    let total_cost_microunits = observations.iter().fold(0_u64, |total, item| {
        total.saturating_add(item.cost_microunits)
    });
    let average_tokens_per_request = average(total_tokens, total_requests);
    let average_cost_microunits_per_request = average(total_cost_microunits, total_requests);
    let metrics = LoadTestMetricsV1 {
        total_requests,
        successful_requests,
        failed_requests,
        success_rate,
        elapsed_ms,
        requests_per_second: if elapsed_ms == 0 {
            total_requests as f64
        } else {
            total_requests as f64 * 1000.0 / elapsed_ms as f64
        },
        p50_duration_ms: percentile(&durations, 50),
        p95_duration_ms: percentile(&durations, 95),
        p99_duration_ms: percentile(&durations, 99),
        total_input_tokens,
        total_output_tokens,
        total_tokens,
        total_cost_microunits,
        average_tokens_per_request,
        average_cost_microunits_per_request,
    };
    let p95_regression_ratio = baseline.map(|baseline| {
        regression_ratio(
            metrics.p95_duration_ms as f64,
            baseline.metrics.p95_duration_ms as f64,
        )
    });
    let token_regression_ratio = baseline.map(|baseline| {
        regression_ratio(
            metrics.average_tokens_per_request,
            baseline.metrics.average_tokens_per_request,
        )
    });
    let cost_regression_ratio = baseline.map(|baseline| {
        regression_ratio(
            metrics.average_cost_microunits_per_request,
            baseline.metrics.average_cost_microunits_per_request,
        )
    });
    let mut reasons = Vec::new();
    if metrics.total_requests == 0 {
        reasons.push("load test produced no measured requests".to_string());
    }
    if plan.duration_seconds.is_none() {
        let expected = plan.concurrency.saturating_mul(plan.requests_per_worker);
        if metrics.total_requests != expected {
            reasons.push(format!(
                "measured request count does not match plan: {} != {expected}",
                metrics.total_requests
            ));
        }
    }
    if metrics.success_rate < plan.min_success_rate {
        reasons.push(format!(
            "success_rate below plan: {:.4} < {:.4}",
            metrics.success_rate, plan.min_success_rate
        ));
    }
    if metrics.p95_duration_ms > plan.max_p95_duration_ms {
        reasons.push(format!(
            "p95_duration_ms above plan: {} > {}",
            metrics.p95_duration_ms, plan.max_p95_duration_ms
        ));
    }
    if metrics.p99_duration_ms > plan.max_p99_duration_ms {
        reasons.push(format!(
            "p99_duration_ms above plan: {} > {}",
            metrics.p99_duration_ms, plan.max_p99_duration_ms
        ));
    }
    check_optional_max(
        &mut reasons,
        "average_tokens_per_request",
        metrics.average_tokens_per_request,
        plan.max_average_tokens_per_request,
    );
    check_optional_max(
        &mut reasons,
        "average_cost_microunits_per_request",
        metrics.average_cost_microunits_per_request,
        plan.max_average_cost_microunits_per_request,
    );
    check_regression(
        &mut reasons,
        "p95_regression_ratio",
        p95_regression_ratio,
        plan.max_p95_regression_ratio,
    );
    check_regression(
        &mut reasons,
        "token_regression_ratio",
        token_regression_ratio,
        plan.max_token_regression_ratio,
    );
    check_regression(
        &mut reasons,
        "cost_regression_ratio",
        cost_regression_ratio,
        plan.max_cost_regression_ratio,
    );
    let failure_samples = observations
        .into_iter()
        .filter_map(|observation| {
            observation.error.map(|error| LoadFailureSampleV1 {
                worker_id: observation.worker_id,
                request_index: observation.request_index,
                http_status: observation.http_status,
                duration_ms: observation.duration_ms,
                session_id: observation.session_id,
                run_id: observation.run_id,
                trace_id: observation.trace_id,
                error,
            })
        })
        .take(MAX_FAILURE_SAMPLES)
        .collect::<Vec<_>>();
    reasons.sort();
    reasons.dedup();
    LoadTestReportV1 {
        schema_version: LOAD_TEST_REPORT_SCHEMA_VERSION.to_string(),
        workload_id: plan.workload_id.clone(),
        generated_at_ms: now_ms(),
        plan_fingerprint: load_test_plan_fingerprint(plan),
        workload_fingerprint: load_test_workload_fingerprint(plan),
        baseline_id: baseline.map(|baseline| baseline.baseline_id.clone()),
        baseline_fingerprint: baseline.map(|baseline| baseline.content_fingerprint.clone()),
        metrics,
        p95_regression_ratio: p95_regression_ratio.flatten(),
        token_regression_ratio: token_regression_ratio.flatten(),
        cost_regression_ratio: cost_regression_ratio.flatten(),
        failure_samples,
        passed: reasons.is_empty(),
        reasons,
    }
}

fn load_test_baseline_fingerprint(baseline: &LoadTestBaselineV1) -> String {
    canonical_json_fingerprint(&json!({
        "schema_version": baseline.schema_version,
        "baseline_id": baseline.baseline_id,
        "workload_id": baseline.workload_id,
        "workload_fingerprint": baseline.workload_fingerprint,
        "accepted_at_ms": baseline.accepted_at_ms,
        "metrics": baseline.metrics,
    }))
}

fn recursive_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(value) = object.get(*key).and_then(Value::as_u64) {
                return Some(value);
            }
        }
        return object.values().find_map(|value| recursive_u64(value, keys));
    }
    value
        .as_array()
        .and_then(|items| items.iter().find_map(|value| recursive_u64(value, keys)))
}

fn recursive_string(value: &Value, keys: &[&str]) -> Option<String> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(value) = object.get(*key).and_then(Value::as_str) {
                return Some(value.to_string());
            }
        }
        return object
            .values()
            .find_map(|value| recursive_string(value, keys));
    }
    value
        .as_array()
        .and_then(|items| items.iter().find_map(|value| recursive_string(value, keys)))
}

fn recursive_cost_microunits(value: &Value) -> u64 {
    if let Some(cost) = recursive_u64(value, &["cost_microunits"]) {
        return cost;
    }
    recursive_f64(value, &["cost"])
        .map(|cost| (cost * 1_000_000.0).round().clamp(0.0, u64::MAX as f64) as u64)
        .unwrap_or(0)
}

fn recursive_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    if let Some(object) = value.as_object() {
        for key in keys {
            if let Some(value) = object.get(*key).and_then(Value::as_f64) {
                return Some(value);
            }
        }
        return object.values().find_map(|value| recursive_f64(value, keys));
    }
    value
        .as_array()
        .and_then(|items| items.iter().find_map(|value| recursive_f64(value, keys)))
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = percentile.saturating_mul(sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn average(total: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        total as f64 / count as f64
    }
}

fn regression_ratio(current: f64, baseline: f64) -> Option<f64> {
    if baseline == 0.0 {
        return (current == 0.0).then_some(0.0);
    }
    Some((current - baseline) / baseline)
}

fn check_optional_max(reasons: &mut Vec<String>, name: &str, value: f64, maximum: Option<f64>) {
    if let Some(maximum) = maximum
        && value > maximum
    {
        reasons.push(format!("{name} above plan: {value:.4} > {maximum:.4}"));
    }
}

fn check_regression(
    reasons: &mut Vec<String>,
    name: &str,
    ratio: Option<Option<f64>>,
    maximum: f64,
) {
    match ratio {
        Some(Some(ratio)) if ratio > maximum => {
            reasons.push(format!("{name} above plan: {ratio:.4} > {maximum:.4}"));
        }
        Some(None) => reasons.push(format!(
            "{name} cannot be bounded because baseline is zero and current is non-zero"
        )),
        _ => {}
    }
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
    use super::{percentile, regression_ratio};

    #[test]
    fn load_percentiles_and_zero_baselines_are_fail_closed() {
        assert_eq!(percentile(&[1, 2, 3, 4, 100], 95), 100);
        assert_eq!(regression_ratio(0.0, 0.0), Some(0.0));
        assert_eq!(regression_ratio(1.0, 0.0), None);
    }
}

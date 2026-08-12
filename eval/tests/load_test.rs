use std::{
    error::Error,
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};

use openagent_eval::{
    LOAD_TEST_PLAN_SCHEMA_VERSION, LoadTestBaselineV1, LoadTestMode, LoadTestPlanV1,
    load_test_baseline_from_report, load_test_workload_fingerprint, run_load_test,
    validate_load_test_baseline, validate_load_test_plan,
};
use serde_json::json;

#[test]
fn load_runner_measures_latency_success_tokens_cost_and_baseline_regression()
-> Result<(), Box<dyn Error>> {
    let (base_url, server) = mock_server(6)?;
    let plan = http_plan(base_url);
    validate_load_test_plan(&plan)?;
    let report = run_load_test(&plan, None)?;
    server.join().expect("mock server");
    assert!(report.passed, "{:?}", report.reasons);
    assert_eq!(report.metrics.total_requests, 6);
    assert_eq!(report.metrics.successful_requests, 6);
    assert_eq!(report.metrics.total_tokens, 18);
    assert_eq!(report.metrics.total_cost_microunits, 30);
    assert_eq!(report.failure_samples.len(), 0);

    let baseline = load_test_baseline_from_report(&report, "local-http-v1", 100)?;
    validate_load_test_baseline(&baseline)?;
    let (base_url, server) = mock_server(6)?;
    let current_plan = http_plan(base_url);
    let current = run_load_test(&current_plan, Some(&baseline))?;
    server.join().expect("mock server");
    assert!(current.passed, "{:?}", current.reasons);
    assert_eq!(current.token_regression_ratio, Some(0.0));
    assert_eq!(current.cost_regression_ratio, Some(0.0));
    Ok(())
}

#[test]
fn load_plan_is_bounded_and_baseline_is_content_addressed() {
    let mut plan = http_plan("http://127.0.0.1:1".to_string());
    plan.concurrency = 0;
    assert!(validate_load_test_plan(&plan).is_err());
    plan.concurrency = 1;
    plan.requests_per_worker = 0;
    assert!(validate_load_test_plan(&plan).is_err());
}

#[test]
fn checked_load_and_soak_plans_and_baseline_are_valid() {
    let plan: LoadTestPlanV1 =
        serde_json::from_str(include_str!("../load/ci-bridge-turn.json")).expect("CI load plan");
    let baseline: LoadTestBaselineV1 =
        serde_json::from_str(include_str!("../load/ci-bridge-turn-baseline.json"))
            .expect("CI load baseline");
    let soak: LoadTestPlanV1 =
        serde_json::from_str(include_str!("../load/staging-soak.json")).expect("soak plan");
    validate_load_test_plan(&plan).expect("valid CI load plan");
    validate_load_test_baseline(&baseline).expect("valid CI load baseline");
    validate_load_test_plan(&soak).expect("valid soak plan");
    assert_eq!(
        baseline.workload_fingerprint,
        load_test_workload_fingerprint(&plan)
    );
    assert_eq!(soak.duration_seconds, Some(1_800));
    assert_eq!(soak.requests_per_worker, 0);
}

fn http_plan(base_url: String) -> LoadTestPlanV1 {
    LoadTestPlanV1 {
        schema_version: LOAD_TEST_PLAN_SCHEMA_VERSION.to_string(),
        workload_id: "http-contract".to_string(),
        mode: LoadTestMode::Http,
        base_url,
        workspace: None,
        auth_token_env: None,
        method: "POST".to_string(),
        path: "/probe".to_string(),
        request_body: json!({"probe": true}),
        concurrency: 2,
        requests_per_worker: 3,
        duration_seconds: None,
        think_time_ms: 0,
        request_timeout_ms: 5_000,
        max_response_bytes: 65_536,
        use_system_proxy: false,
        min_success_rate: 1.0,
        max_p95_duration_ms: 5_000,
        max_p99_duration_ms: 5_000,
        max_average_tokens_per_request: Some(3.0),
        max_average_cost_microunits_per_request: Some(5.0),
        max_p95_regression_ratio: 10.0,
        max_token_regression_ratio: 0.0,
        max_cost_regression_ratio: 0.0,
    }
}

fn mock_server(requests: usize) -> Result<(String, thread::JoinHandle<()>), Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let server = thread::spawn(move || {
        let started = Instant::now();
        let mut served = 0_usize;
        while served < requests && started.elapsed() < Duration::from_secs(10) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0_u8; 8192];
                    let _ = stream.read(&mut request).expect("read mock request");
                    let body = r#"{"status":"ok","usage":{"input_tokens":2,"output_tokens":1},"cost_microunits":5}"#;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("write mock response");
                    served += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept mock connection: {error}"),
            }
        }
        assert_eq!(served, requests, "mock server request count");
    });
    Ok((format!("http://{address}"), server))
}

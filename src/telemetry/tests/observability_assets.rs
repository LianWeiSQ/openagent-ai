use std::{collections::BTreeSet, fs, path::PathBuf};

use serde_json::Value;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn grafana_dashboard_is_valid_and_covers_the_operational_contract() {
    let path = workspace_root().join("observability/grafana/dashboards/openharness-runtime.json");
    let raw = fs::read_to_string(path).expect("read Grafana dashboard");
    let dashboard: Value = serde_json::from_str(&raw).expect("valid Grafana dashboard JSON");
    assert_eq!(dashboard["uid"], "openharness-runtime-slo");
    assert_eq!(dashboard["title"], "OpenHarness Runtime & SLO");

    let required_metrics = [
        "openharness_runs_total",
        "openharness_run_duration_seconds_bucket",
        "openharness_stage_duration_seconds_bucket",
        "openharness_queue_wait_seconds_bucket",
        "openharness_provider_requests_total",
        "openharness_provider_duration_seconds_bucket",
        "openharness_llm_tokens_total",
        "openharness_llm_cost_total",
        "openharness_tool_calls_total",
        "openharness_retries_total",
        "openharness_fallbacks_total",
        "openharness_active_workers",
        "openharness_queue_depth",
        "openharness_telemetry_export_failures_total",
        "openharness_trace_completeness_ratio_sum",
    ];
    for metric in required_metrics {
        assert!(raw.contains(metric), "dashboard is missing {metric}");
    }
    for forbidden in openagent_telemetry::FORBIDDEN_METRIC_LABELS {
        assert!(
            !raw.contains(&format!("{forbidden}=~")) && !raw.contains(&format!("{forbidden}=\"")),
            "dashboard queries use forbidden label {forbidden}"
        );
    }

    let panel_ids = dashboard["panels"]
        .as_array()
        .expect("dashboard panels")
        .iter()
        .filter_map(|panel| panel["id"].as_u64())
        .collect::<Vec<_>>();
    let unique_ids = panel_ids.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        panel_ids.len(),
        unique_ids.len(),
        "panel ids must be unique"
    );
}

#[test]
fn prometheus_rules_are_valid_and_have_bounded_alert_contracts() {
    let path = workspace_root().join("observability/prometheus/openharness-alerts.yaml");
    let raw = fs::read_to_string(path).expect("read Prometheus rules");
    let rules: serde_yaml::Value = serde_yaml::from_str(&raw).expect("valid Prometheus rule YAML");
    let groups = rules
        .get("groups")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("rule groups");
    assert!(groups.len() >= 2);
    for required in [
        "OpenHarnessRunSLOBurnCritical",
        "OpenHarnessRunLatencyP95High",
        "OpenHarnessQueueWaitP95High",
        "OpenHarnessTraceCompletenessLow",
        "OpenHarnessTelemetryExportFailures",
    ] {
        assert!(raw.contains(required), "missing alert {required}");
    }
    for forbidden in openagent_telemetry::FORBIDDEN_METRIC_LABELS {
        assert!(
            !raw.contains(&format!("{forbidden}=~")) && !raw.contains(&format!("{forbidden}=\"")),
            "alert queries use forbidden label {forbidden}"
        );
    }
}

#[test]
fn deployment_bundle_is_pinned_private_and_correlation_ready() {
    let root = workspace_root().join("observability");
    let compose_raw =
        fs::read_to_string(root.join("deploy/compose.yaml")).expect("read observability compose");
    let compose: serde_yaml::Value =
        serde_yaml::from_str(&compose_raw).expect("valid observability compose YAML");
    let services = compose
        .get("services")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("compose services");
    for service in ["otel-collector", "prometheus", "tempo", "loki", "grafana"] {
        assert!(
            services.contains_key(serde_yaml::Value::String(service.to_string())),
            "compose is missing {service}"
        );
    }
    assert!(!compose_raw.contains(":latest"));
    assert!(compose_raw.contains("OBSERVABILITY_BIND_ADDRESS:-127.0.0.1"));
    assert!(compose_raw.contains("internal: true"));
    assert!(compose_raw.contains("/run/secrets/openharness-metrics-token:ro"));
    assert!(compose_raw.contains("/run/secrets/grafana-admin-password:ro"));

    let collector_raw =
        fs::read_to_string(root.join("deploy/otel-collector.yaml")).expect("read collector config");
    let collector: serde_yaml::Value =
        serde_yaml::from_str(&collector_raw).expect("valid collector YAML");
    let pipelines = collector
        .get("service")
        .and_then(|service| service.get("pipelines"))
        .and_then(serde_yaml::Value::as_mapping)
        .expect("collector pipelines");
    for pipeline in ["traces", "logs/otlp", "logs/ledger"] {
        assert!(
            pipelines.contains_key(serde_yaml::Value::String(pipeline.to_string())),
            "collector is missing {pipeline} pipeline"
        );
    }
    for sensitive in [
        "gen_ai.prompt",
        "gen_ai.completion",
        "http.request.header.authorization",
        "enduser.id",
    ] {
        assert!(
            collector_raw.contains(sensitive),
            "collector privacy processor is missing {sensitive}"
        );
    }
    assert!(collector_raw.contains("/events.jsonl"));
    assert!(collector_raw.contains("http://tempo:4318"));
    assert!(collector_raw.contains("http://loki:3100/otlp"));

    let prometheus_raw =
        fs::read_to_string(root.join("deploy/prometheus.yaml")).expect("read Prometheus config");
    serde_yaml::from_str::<serde_yaml::Value>(&prometheus_raw).expect("valid Prometheus YAML");
    assert!(prometheus_raw.contains("credentials_file"));
    assert!(prometheus_raw.contains("host.docker.internal:8787"));

    for config in ["tempo.yaml", "loki.yaml"] {
        let raw = fs::read_to_string(root.join("deploy").join(config))
            .unwrap_or_else(|error| panic!("read {config}: {error}"));
        serde_yaml::from_str::<serde_yaml::Value>(&raw)
            .unwrap_or_else(|error| panic!("valid {config}: {error}"));
    }

    let datasources_raw =
        fs::read_to_string(root.join("grafana/provisioning/datasources/prometheus.yaml"))
            .expect("read Grafana datasources");
    serde_yaml::from_str::<serde_yaml::Value>(&datasources_raw)
        .expect("valid Grafana datasource YAML");
    for uid in ["prometheus", "tempo", "loki"] {
        assert!(
            datasources_raw.contains(&format!("uid: {uid}")),
            "missing Grafana datasource {uid}"
        );
    }
    assert!(datasources_raw.contains("trace_id"));
    assert!(datasources_raw.contains("datasourceUid: tempo"));
}

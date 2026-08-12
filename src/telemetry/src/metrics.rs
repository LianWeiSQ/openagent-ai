use std::{error::Error, fmt};

use opentelemetry::{
    KeyValue,
    metrics::{Counter, Histogram, UpDownCounter},
};

use crate::{OutcomeReason, RunOutcome, RunSurface};

pub const FORBIDDEN_METRIC_LABELS: &[&str] = &[
    "run_id",
    "session_id",
    "task_id",
    "user_id",
    "workspace",
    "path",
    "prompt",
    "input",
    "output",
    "error_message",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetricDimensions {
    pub surface: RunSurface,
    pub agent_name: String,
    pub agent_version: String,
    pub harness_version: String,
    pub environment: String,
}

impl MetricDimensions {
    #[must_use]
    pub fn attributes(&self) -> Vec<KeyValue> {
        vec![
            KeyValue::new("surface", bounded_label(self.surface.as_str())),
            KeyValue::new("agent_name", bounded_label(&self.agent_name)),
            KeyValue::new("agent_version", bounded_label(&self.agent_version)),
            KeyValue::new("harness_version", bounded_label(&self.harness_version)),
            KeyValue::new("deployment_environment", bounded_label(&self.environment)),
        ]
    }
}

#[derive(Clone, Debug)]
pub struct OpenHarnessMetrics {
    runs: Counter<u64>,
    run_duration: Histogram<f64>,
    stage_duration: Histogram<f64>,
    queue_wait: Histogram<f64>,
    provider_requests: Counter<u64>,
    provider_duration: Histogram<f64>,
    llm_tokens: Counter<u64>,
    llm_cost: Counter<f64>,
    tool_calls: Counter<u64>,
    tool_duration: Histogram<f64>,
    mcp_calls: Counter<u64>,
    retries: Counter<u64>,
    fallbacks: Counter<u64>,
    degraded_runs: Counter<u64>,
    active_workers: UpDownCounter<i64>,
    queue_depth: UpDownCounter<i64>,
    telemetry_export_failures: Counter<u64>,
    trace_completeness: Histogram<f64>,
}

impl OpenHarnessMetrics {
    pub(crate) fn new(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
            runs: meter
                .u64_counter("openharness_runs")
                .with_description("Agent runs by terminal outcome")
                .build(),
            run_duration: meter
                .f64_histogram("openharness_run_duration")
                .with_unit("s")
                .with_description("End-to-end agent run duration")
                .build(),
            stage_duration: meter
                .f64_histogram("openharness_stage_duration")
                .with_unit("s")
                .with_description("Duration of a bounded runtime stage")
                .build(),
            queue_wait: meter
                .f64_histogram("openharness_queue_wait")
                .with_unit("s")
                .with_description("Time spent waiting for a turn worker")
                .build(),
            provider_requests: meter
                .u64_counter("openharness_provider_requests")
                .with_description("Provider requests by bounded provider and status")
                .build(),
            provider_duration: meter
                .f64_histogram("openharness_provider_duration")
                .with_unit("s")
                .with_description("Provider request duration")
                .build(),
            llm_tokens: meter
                .u64_counter("openharness_llm_tokens")
                .with_description("LLM token usage by direction")
                .build(),
            llm_cost: meter
                .f64_counter("openharness_llm_cost")
                .with_description("Estimated LLM cost in configured currency units")
                .build(),
            tool_calls: meter
                .u64_counter("openharness_tool_calls")
                .with_description("Tool calls by bounded tool class and status")
                .build(),
            tool_duration: meter
                .f64_histogram("openharness_tool_duration")
                .with_unit("s")
                .with_description("Tool execution duration")
                .build(),
            mcp_calls: meter
                .u64_counter("openharness_mcp_calls")
                .with_description("MCP calls by status")
                .build(),
            retries: meter
                .u64_counter("openharness_retries")
                .with_description("Bounded retry attempts by phase and reason")
                .build(),
            fallbacks: meter
                .u64_counter("openharness_fallbacks")
                .with_description("Provider or model fallback transitions")
                .build(),
            degraded_runs: meter
                .u64_counter("openharness_degraded_runs")
                .with_description("Runs that completed with degraded outcome")
                .build(),
            active_workers: meter
                .i64_up_down_counter("openharness_active_workers")
                .with_description("Current active turn workers")
                .build(),
            queue_depth: meter
                .i64_up_down_counter("openharness_queue_depth")
                .with_description("Current queued turn count")
                .build(),
            telemetry_export_failures: meter
                .u64_counter("openharness_telemetry_export_failures")
                .with_description("Asynchronous telemetry export failures")
                .build(),
            trace_completeness: meter
                .f64_histogram("openharness_trace_completeness_ratio")
                .with_description("Critical span completeness ratio per sampled run")
                .build(),
        }
    }

    pub fn record_run(
        &self,
        dimensions: &MetricDimensions,
        outcome: RunOutcome,
        reason: OutcomeReason,
        duration_seconds: f64,
    ) {
        let mut attributes = dimensions.attributes();
        attributes.push(KeyValue::new("outcome", outcome.as_str()));
        attributes.push(KeyValue::new("reason_code", reason.as_str()));
        self.runs.add(1, &attributes);
        self.run_duration
            .record(non_negative(duration_seconds), &attributes);
        if outcome == RunOutcome::Degraded {
            self.degraded_runs.add(1, &attributes);
        }
    }

    pub fn record_stage(&self, phase: &str, status: &str, duration_seconds: f64) {
        self.stage_duration.record(
            non_negative(duration_seconds),
            &[
                KeyValue::new("phase", bounded_label(phase)),
                KeyValue::new("status", bounded_label(status)),
            ],
        );
    }

    pub fn record_queue_wait(&self, status: &str, duration_seconds: f64) {
        self.queue_wait.record(
            non_negative(duration_seconds),
            &[KeyValue::new("status", bounded_label(status))],
        );
    }

    pub fn record_provider(
        &self,
        provider: &str,
        model_family: &str,
        status: &str,
        duration_seconds: f64,
    ) {
        let attributes = [
            KeyValue::new("provider", bounded_label(provider)),
            KeyValue::new("model_family", bounded_label(model_family)),
            KeyValue::new("status", bounded_label(status)),
        ];
        self.provider_requests.add(1, &attributes);
        self.provider_duration
            .record(non_negative(duration_seconds), &attributes);
    }

    pub fn add_tokens(&self, provider: &str, model_family: &str, direction: &str, count: u64) {
        self.llm_tokens.add(
            count,
            &[
                KeyValue::new("provider", bounded_label(provider)),
                KeyValue::new("model_family", bounded_label(model_family)),
                KeyValue::new("direction", bounded_label(direction)),
            ],
        );
    }

    pub fn add_cost(&self, provider: &str, model_family: &str, amount: f64) {
        self.llm_cost.add(
            non_negative(amount),
            &[
                KeyValue::new("provider", bounded_label(provider)),
                KeyValue::new("model_family", bounded_label(model_family)),
            ],
        );
    }

    pub fn record_tool(&self, tool_class: &str, status: &str, duration_seconds: f64) {
        let attributes = [
            KeyValue::new("tool_class", bounded_label(tool_class)),
            KeyValue::new("status", bounded_label(status)),
        ];
        self.tool_calls.add(1, &attributes);
        self.tool_duration
            .record(non_negative(duration_seconds), &attributes);
    }

    pub fn record_mcp(&self, status: &str) {
        self.mcp_calls
            .add(1, &[KeyValue::new("status", bounded_label(status))]);
    }

    pub fn record_retry(&self, phase: &str, reason: &str) {
        self.retries.add(
            1,
            &[
                KeyValue::new("phase", bounded_label(phase)),
                KeyValue::new("reason_code", bounded_label(reason)),
            ],
        );
    }

    pub fn record_fallback(&self, provider: &str, reason: &str) {
        self.fallbacks.add(
            1,
            &[
                KeyValue::new("provider", bounded_label(provider)),
                KeyValue::new("reason_code", bounded_label(reason)),
            ],
        );
    }

    pub fn add_active_workers(&self, delta: i64) {
        self.active_workers.add(delta, &[]);
    }

    pub fn add_queue_depth(&self, delta: i64) {
        self.queue_depth.add(delta, &[]);
    }

    pub fn record_export_failures(&self, signal: &str, count: u64) {
        self.telemetry_export_failures
            .add(count, &[KeyValue::new("signal", bounded_label(signal))]);
    }

    pub fn record_trace_completeness(&self, ratio: f64) {
        self.trace_completeness.record(ratio.clamp(0.0, 1.0), &[]);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetricError {
    ForbiddenLabel(String),
    InvalidLabel(String),
}

impl fmt::Display for MetricError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForbiddenLabel(label) => write!(formatter, "forbidden metric label: {label}"),
            Self::InvalidLabel(label) => write!(formatter, "invalid metric label: {label}"),
        }
    }
}

impl Error for MetricError {}

pub fn validate_metric_label_key(key: &str) -> Result<(), MetricError> {
    if FORBIDDEN_METRIC_LABELS.contains(&key) {
        return Err(MetricError::ForbiddenLabel(key.to_string()));
    }
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(MetricError::InvalidLabel(key.to_string()));
    }
    Ok(())
}

fn bounded_label(value: &str) -> String {
    let normalized = value
        .trim()
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized
    }
}

fn non_negative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_label_policy_rejects_high_cardinality_and_content_keys() {
        for key in FORBIDDEN_METRIC_LABELS {
            assert!(validate_metric_label_key(key).is_err());
        }
        assert!(validate_metric_label_key("provider").is_ok());
        assert!(validate_metric_label_key("reason_code").is_ok());
    }

    #[test]
    fn bounded_labels_never_expose_unbounded_values() {
        assert_eq!(bounded_label("  OpenAI/GPT  "), "openai_gpt");
        assert_eq!(bounded_label(""), "unknown");
        assert_eq!(bounded_label(&"x".repeat(80)).len(), 64);
    }
}

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use opentelemetry::{
    InstrumentationScope, KeyValue,
    metrics::MeterProvider as _,
    trace::{Span as _, SpanKind as OtelSpanKind, Status, Tracer as _, TracerProvider as _},
};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{
    Resource,
    metrics::SdkMeterProvider,
    trace::{
        BatchConfigBuilder, BatchSpanProcessor, Sampler, SdkTracer, SdkTracerProvider,
        Span as SdkSpan, SpanData, SpanExporter,
    },
};
use prometheus::{Encoder, Registry, TextEncoder};

use crate::{
    ExecutionState, INSTRUMENTATION_SCOPE, OpenHarnessMetrics, OutcomeReason, RunOutcome,
    TelemetryAttributes, TraceContext,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpanKind {
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl From<SpanKind> for OtelSpanKind {
    fn from(value: SpanKind) -> Self {
        match value {
            SpanKind::Internal => Self::Internal,
            SpanKind::Server => Self::Server,
            SpanKind::Client => Self::Client,
            SpanKind::Producer => Self::Producer,
            SpanKind::Consumer => Self::Consumer,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub service_name: String,
    pub service_version: String,
    pub environment: String,
    pub otlp_endpoint: Option<String>,
    pub trace_sample_ratio: f64,
    pub export_timeout_ms: u64,
    pub batch_delay_ms: u64,
    pub max_queue_size: usize,
    pub prometheus_enabled: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: "openharness".to_string(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            environment: "local".to_string(),
            otlp_endpoint: None,
            trace_sample_ratio: 1.0,
            export_timeout_ms: 3_000,
            batch_delay_ms: 1_000,
            max_queue_size: 2_048,
            prometheus_enabled: false,
        }
    }
}

impl TelemetryConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.otlp_endpoint = non_empty_env("OTEL_EXPORTER_OTLP_ENDPOINT");
        config.enabled =
            bool_env("OPENHARNESS_TELEMETRY_ENABLED").unwrap_or(config.otlp_endpoint.is_some());
        config.prometheus_enabled = bool_env("OPENHARNESS_PROMETHEUS_ENABLED").unwrap_or(false);
        config.service_name = non_empty_env("OTEL_SERVICE_NAME").unwrap_or(config.service_name);
        config.service_version =
            non_empty_env("OPENHARNESS_SERVICE_VERSION").unwrap_or(config.service_version);
        config.environment = non_empty_env("OTEL_DEPLOYMENT_ENVIRONMENT")
            .or_else(|| non_empty_env("OPENHARNESS_ENVIRONMENT"))
            .unwrap_or(config.environment);
        config.trace_sample_ratio = number_env("OTEL_TRACES_SAMPLER_ARG")
            .unwrap_or(config.trace_sample_ratio)
            .clamp(0.0, 1.0);
        config.export_timeout_ms = integer_env("OTEL_EXPORTER_OTLP_TIMEOUT")
            .unwrap_or(config.export_timeout_ms)
            .clamp(100, 60_000);
        config.batch_delay_ms = integer_env("OTEL_BSP_SCHEDULE_DELAY")
            .unwrap_or(config.batch_delay_ms)
            .clamp(100, 60_000);
        config.max_queue_size = integer_env("OTEL_BSP_MAX_QUEUE_SIZE")
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(config.max_queue_size)
            .clamp(128, 65_536);
        config
    }

    pub fn validate(&self) -> Result<(), TelemetryRuntimeError> {
        if self.service_name.trim().is_empty() {
            return Err(TelemetryRuntimeError::InvalidConfig(
                "service_name must not be empty".to_string(),
            ));
        }
        if !self.trace_sample_ratio.is_finite() || !(0.0..=1.0).contains(&self.trace_sample_ratio) {
            return Err(TelemetryRuntimeError::InvalidConfig(
                "trace_sample_ratio must be between 0 and 1".to_string(),
            ));
        }
        if self.max_queue_size == 0 {
            return Err(TelemetryRuntimeError::InvalidConfig(
                "max_queue_size must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetryHealth {
    pub enabled: bool,
    pub trace_export_configured: bool,
    pub prometheus_configured: bool,
    pub trace_export_failures: u64,
    pub shutdown_failures: u64,
}

#[derive(Clone, Debug)]
pub struct TelemetryRuntime {
    config: TelemetryConfig,
    tracer_provider: Option<SdkTracerProvider>,
    tracer: Option<SdkTracer>,
    meter_provider: Option<SdkMeterProvider>,
    metrics: Option<OpenHarnessMetrics>,
    registry: Option<Registry>,
    trace_export_failures: Arc<AtomicU64>,
    shutdown_failures: Arc<AtomicU64>,
}

impl TelemetryRuntime {
    pub fn initialize(config: TelemetryConfig) -> Result<Self, TelemetryRuntimeError> {
        config.validate()?;
        let trace_export_failures = Arc::new(AtomicU64::new(0));
        let shutdown_failures = Arc::new(AtomicU64::new(0));
        let resource = telemetry_resource(&config);

        let (tracer_provider, tracer) = if config.enabled {
            if let Some(endpoint) = config.otlp_endpoint.as_deref() {
                let exporter = opentelemetry_otlp::SpanExporter::builder()
                    .with_http()
                    .with_protocol(Protocol::HttpBinary)
                    .with_endpoint(endpoint)
                    .with_timeout(Duration::from_millis(config.export_timeout_ms))
                    .build()
                    .map_err(|error| TelemetryRuntimeError::Exporter(error.to_string()))?;
                let exporter =
                    FailureCountingExporter::new(exporter, Arc::clone(&trace_export_failures));
                let batch = BatchSpanProcessor::builder(exporter)
                    .with_batch_config(
                        BatchConfigBuilder::default()
                            .with_max_queue_size(config.max_queue_size)
                            .with_scheduled_delay(Duration::from_millis(config.batch_delay_ms))
                            .build(),
                    )
                    .build();
                let provider = SdkTracerProvider::builder()
                    .with_span_processor(batch)
                    .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                        config.trace_sample_ratio,
                    ))))
                    .with_resource(resource.clone())
                    .build();
                let tracer = provider.tracer_with_scope(
                    InstrumentationScope::builder(INSTRUMENTATION_SCOPE)
                        .with_version(config.service_version.clone())
                        .build(),
                );
                (Some(provider), Some(tracer))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        let (meter_provider, metrics, registry) = if config.prometheus_enabled {
            let registry = Registry::new();
            let exporter = opentelemetry_prometheus::exporter()
                .with_registry(registry.clone())
                .build()
                .map_err(|error| TelemetryRuntimeError::Metrics(error.to_string()))?;
            let provider = SdkMeterProvider::builder()
                .with_reader(exporter)
                .with_resource(resource)
                .build();
            let meter = provider.meter_with_scope(
                InstrumentationScope::builder(INSTRUMENTATION_SCOPE)
                    .with_version(config.service_version.clone())
                    .build(),
            );
            let metrics = OpenHarnessMetrics::new(&meter);
            (Some(provider), Some(metrics), Some(registry))
        } else {
            (None, None, None)
        };

        Ok(Self {
            config,
            tracer_provider,
            tracer,
            meter_provider,
            metrics,
            registry,
            trace_export_failures,
            shutdown_failures,
        })
    }

    #[must_use]
    pub fn config(&self) -> &TelemetryConfig {
        &self.config
    }

    #[must_use]
    pub fn metrics(&self) -> Option<&OpenHarnessMetrics> {
        self.metrics.as_ref()
    }

    #[must_use]
    pub fn health(&self) -> TelemetryHealth {
        TelemetryHealth {
            enabled: self.config.enabled || self.config.prometheus_enabled,
            trace_export_configured: self.tracer.is_some(),
            prometheus_configured: self.metrics.is_some(),
            trace_export_failures: self.trace_export_failures.load(Ordering::Relaxed),
            shutdown_failures: self.shutdown_failures.load(Ordering::Relaxed),
        }
    }

    pub fn start_span(
        &self,
        name: impl Into<String>,
        kind: SpanKind,
        parent: Option<&TraceContext>,
        attributes: TelemetryAttributes,
    ) -> Result<OpenHarnessSpan, TelemetryRuntimeError> {
        self.start_span_inner(name.into(), kind, parent, attributes, None)
    }

    pub fn start_span_at(
        &self,
        name: impl Into<String>,
        kind: SpanKind,
        parent: Option<&TraceContext>,
        attributes: TelemetryAttributes,
        started_at: SystemTime,
    ) -> Result<OpenHarnessSpan, TelemetryRuntimeError> {
        self.start_span_inner(name.into(), kind, parent, attributes, Some(started_at))
    }

    fn start_span_inner(
        &self,
        name: String,
        kind: SpanKind,
        parent: Option<&TraceContext>,
        attributes: TelemetryAttributes,
        started_at: Option<SystemTime>,
    ) -> Result<OpenHarnessSpan, TelemetryRuntimeError> {
        let fallback_context = parent.map_or_else(
            || TraceContext::new_root(self.config.trace_sample_ratio > 0.0),
            TraceContext::child,
        );
        let Some(tracer) = self.tracer.as_ref() else {
            return Ok(OpenHarnessSpan {
                span: None,
                context: fallback_context,
                ended: false,
            });
        };
        let parent_context = parent
            .map(TraceContext::to_otel_context)
            .transpose()
            .map_err(|error| TelemetryRuntimeError::Propagation(error.to_string()))?
            .unwrap_or_default();
        let mut builder = tracer
            .span_builder(name)
            .with_kind(kind.into())
            .with_attributes(attributes.into_key_values());
        if let Some(started_at) = started_at {
            builder = builder.with_start_time(started_at);
        }
        let span = tracer.build_with_context(builder, &parent_context);
        let context = TraceContext::from_span_context(span.span_context());
        Ok(OpenHarnessSpan {
            span: Some(span),
            context,
            ended: false,
        })
    }

    pub fn prometheus_text(&self) -> Result<Option<String>, TelemetryRuntimeError> {
        let Some(registry) = self.registry.as_ref() else {
            return Ok(None);
        };
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        encoder
            .encode(&registry.gather(), &mut buffer)
            .map_err(|error| TelemetryRuntimeError::Metrics(error.to_string()))?;
        String::from_utf8(buffer)
            .map(Some)
            .map_err(|error| TelemetryRuntimeError::Metrics(error.to_string()))
    }

    pub fn force_flush(&self) -> TelemetryHealth {
        if let Some(provider) = self.tracer_provider.as_ref()
            && provider.force_flush().is_err()
        {
            self.shutdown_failures.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(provider) = self.meter_provider.as_ref()
            && provider.force_flush().is_err()
        {
            self.shutdown_failures.fetch_add(1, Ordering::Relaxed);
        }
        self.publish_export_failures();
        self.health()
    }

    pub fn shutdown(&self) -> TelemetryHealth {
        if let Some(provider) = self.tracer_provider.as_ref()
            && provider
                .shutdown_with_timeout(Duration::from_millis(self.config.export_timeout_ms))
                .is_err()
        {
            self.shutdown_failures.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(provider) = self.meter_provider.as_ref()
            && provider.shutdown().is_err()
        {
            self.shutdown_failures.fetch_add(1, Ordering::Relaxed);
        }
        self.publish_export_failures();
        self.health()
    }

    fn publish_export_failures(&self) {
        let failures = self.trace_export_failures.load(Ordering::Relaxed);
        if failures > 0
            && let Some(metrics) = self.metrics.as_ref()
        {
            metrics.record_export_failures("trace", failures);
        }
    }
}

#[derive(Debug)]
pub struct OpenHarnessSpan {
    span: Option<SdkSpan>,
    context: TraceContext,
    ended: bool,
}

impl OpenHarnessSpan {
    #[must_use]
    pub fn context(&self) -> &TraceContext {
        &self.context
    }

    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<String>) {
        if let Some(span) = self.span.as_mut() {
            let attributes = TelemetryAttributes::new().insert(key, value);
            for attribute in attributes.into_key_values() {
                span.set_attribute(attribute);
            }
        }
    }

    pub fn add_event(&mut self, name: impl Into<String>, attributes: TelemetryAttributes) {
        if let Some(span) = self.span.as_mut() {
            span.add_event(name.into(), attributes.into_key_values());
        }
    }

    pub fn end(mut self, outcome: RunOutcome, reason: OutcomeReason) {
        let state = match outcome {
            RunOutcome::Success | RunOutcome::Degraded => ExecutionState::Completed,
            RunOutcome::Failed => ExecutionState::Failed,
            RunOutcome::Cancelled => ExecutionState::Cancelled,
            RunOutcome::Interrupted => ExecutionState::Interrupted,
        };
        self.finish(state, Some(outcome), reason);
    }

    /// Ends a span at a durable execution-state boundary.
    ///
    /// Waiting and running segments are intentionally allowed to end without a
    /// terminal run outcome. This keeps approval/question pauses observable
    /// without incorrectly counting them as successful or failed runs.
    pub fn end_state(
        mut self,
        state: ExecutionState,
        outcome: Option<RunOutcome>,
        reason: OutcomeReason,
    ) {
        self.finish(state, outcome, reason);
    }

    fn finish(
        &mut self,
        state: ExecutionState,
        outcome: Option<RunOutcome>,
        reason: OutcomeReason,
    ) {
        if self.ended {
            return;
        }
        if let Some(span) = self.span.as_mut() {
            span.set_attribute(KeyValue::new("execution.state", state.as_str()));
            if let Some(outcome) = outcome.as_ref() {
                span.set_attribute(KeyValue::new("run.outcome", outcome.as_str()));
            }
            span.set_attribute(KeyValue::new("run.reason_code", reason.as_str()));
            match state {
                ExecutionState::Queued
                | ExecutionState::Running
                | ExecutionState::Waiting
                | ExecutionState::Completed => span.set_status(Status::Ok),
                ExecutionState::Failed
                | ExecutionState::Cancelled
                | ExecutionState::Interrupted => {
                    span.set_status(Status::error(reason.as_str().to_string()));
                }
            }
            span.end();
        }
        self.ended = true;
    }
}

impl Drop for OpenHarnessSpan {
    fn drop(&mut self) {
        if !self.ended {
            self.finish(
                ExecutionState::Interrupted,
                Some(RunOutcome::Interrupted),
                OutcomeReason::ProcessInterrupted,
            );
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TelemetryRuntimeError {
    InvalidConfig(String),
    Exporter(String),
    Metrics(String),
    Propagation(String),
}

impl fmt::Display for TelemetryRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(error) => {
                write!(formatter, "invalid telemetry configuration: {error}")
            }
            Self::Exporter(error) => {
                write!(
                    formatter,
                    "telemetry exporter initialization failed: {error}"
                )
            }
            Self::Metrics(error) => write!(formatter, "telemetry metrics failure: {error}"),
            Self::Propagation(error) => {
                write!(formatter, "telemetry propagation failure: {error}")
            }
        }
    }
}

impl Error for TelemetryRuntimeError {}

#[derive(Debug)]
struct FailureCountingExporter<E> {
    inner: E,
    failures: Arc<AtomicU64>,
}

impl<E> FailureCountingExporter<E> {
    fn new(inner: E, failures: Arc<AtomicU64>) -> Self {
        Self { inner, failures }
    }
}

impl<E> SpanExporter for FailureCountingExporter<E>
where
    E: SpanExporter,
{
    async fn export(&self, batch: Vec<SpanData>) -> opentelemetry_sdk::error::OTelSdkResult {
        let result = self.inner.export(batch).await;
        if result.is_err() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

fn telemetry_resource(config: &TelemetryConfig) -> Resource {
    Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attributes([
            KeyValue::new("service.version", config.service_version.clone()),
            KeyValue::new("deployment.environment.name", config.environment.clone()),
            KeyValue::new("telemetry.schema.version", crate::TELEMETRY_SCHEMA_VERSION),
        ])
        .build()
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn bool_env(key: &str) -> Option<bool> {
    non_empty_env(key).and_then(|value| match value.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Some(true),
        "0" | "off" | "false" | "no" => Some(false),
        _ => None,
    })
}

fn integer_env(key: &str) -> Option<u64> {
    non_empty_env(key).and_then(|value| value.parse().ok())
}

fn number_env(key: &str) -> Option<f64> {
    non_empty_env(key).and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use crate::{MetricDimensions, RunSurface};

    use super::*;

    #[test]
    fn disabled_runtime_still_produces_valid_correlated_context() -> Result<(), Box<dyn Error>> {
        let runtime = TelemetryRuntime::initialize(TelemetryConfig::default())?;
        let root = runtime.start_span(
            "agent.run",
            SpanKind::Server,
            None,
            TelemetryAttributes::new().insert("prompt", "must redact"),
        )?;
        root.context().validate()?;
        let child = runtime.start_span(
            "gen_ai.request",
            SpanKind::Client,
            Some(root.context()),
            TelemetryAttributes::new(),
        )?;
        assert_eq!(root.context().trace_id, child.context().trace_id);
        assert_ne!(root.context().span_id, child.context().span_id);
        child.end(RunOutcome::Success, OutcomeReason::None);
        root.end(RunOutcome::Success, OutcomeReason::None);
        Ok(())
    }

    #[test]
    fn prometheus_runtime_exports_only_bounded_dimensions() -> Result<(), Box<dyn Error>> {
        let runtime = TelemetryRuntime::initialize(TelemetryConfig {
            prometheus_enabled: true,
            ..TelemetryConfig::default()
        })?;
        let metrics = runtime.metrics().ok_or_else(|| {
            TelemetryRuntimeError::Metrics("metrics were not initialized".to_string())
        })?;
        metrics.record_run(
            &MetricDimensions {
                surface: RunSurface::Http,
                agent_name: "server".to_string(),
                agent_version: "agent-v1".to_string(),
                harness_version: "0.1.0".to_string(),
                environment: "test".to_string(),
            },
            RunOutcome::Degraded,
            OutcomeReason::ProviderFallback,
            1.25,
        );
        metrics.record_stage("provider", "success", 0.5);
        metrics.record_queue_wait("dispatched", 0.25);
        metrics.record_provider("openai", "gpt", "success", 0.4);
        metrics.add_tokens("openai", "gpt", "input", 10);
        metrics.add_cost("openai", "gpt", 0.01);
        metrics.record_tool("builtin", "success", 0.1);
        metrics.record_mcp("success");
        metrics.record_retry("provider", "transient_error");
        metrics.record_fallback("openai", "provider_error");
        metrics.add_active_workers(1);
        metrics.add_queue_depth(1);
        metrics.record_export_failures("trace", 1);
        metrics.record_trace_completeness(1.0);
        let text = runtime.prometheus_text()?.ok_or_else(|| {
            TelemetryRuntimeError::Metrics("prometheus output was disabled".to_string())
        })?;
        for required in [
            "openharness_runs_total",
            "openharness_run_duration_seconds_bucket",
            "openharness_stage_duration_seconds_bucket",
            "openharness_queue_wait_seconds_bucket",
            "openharness_provider_requests_total",
            "openharness_provider_duration_seconds_bucket",
            "openharness_llm_tokens_total",
            "openharness_llm_cost_total",
            "openharness_tool_calls_total",
            "openharness_tool_duration_seconds_bucket",
            "openharness_mcp_calls_total",
            "openharness_retries_total",
            "openharness_fallbacks_total",
            "openharness_degraded_runs_total",
            "openharness_active_workers",
            "openharness_queue_depth",
            "openharness_telemetry_export_failures_total",
            "openharness_trace_completeness_ratio_sum",
        ] {
            assert!(
                text.contains(required),
                "missing metric {required} in:\n{text}"
            );
        }
        assert!(text.contains("outcome=\"degraded\""));
        for forbidden in crate::FORBIDDEN_METRIC_LABELS {
            assert!(!text.contains(&format!("{forbidden}=\"")));
        }
        Ok(())
    }

    #[test]
    fn collector_outage_is_fail_open_and_counted() -> Result<(), Box<dyn Error>> {
        let runtime = TelemetryRuntime::initialize(TelemetryConfig {
            enabled: true,
            otlp_endpoint: Some("http://127.0.0.1:1/v1/traces".to_string()),
            export_timeout_ms: 100,
            batch_delay_ms: 100,
            max_queue_size: 128,
            prometheus_enabled: true,
            ..TelemetryConfig::default()
        })?;
        let span = runtime.start_span(
            "fault.collector_outage",
            SpanKind::Internal,
            None,
            TelemetryAttributes::new(),
        )?;
        span.end(RunOutcome::Success, OutcomeReason::None);
        let health = runtime.force_flush();
        assert!(health.enabled);
        assert!(health.trace_export_configured);
        assert!(health.trace_export_failures >= 1 || health.shutdown_failures >= 1);
        let metrics = runtime.prometheus_text()?.unwrap_or_default();
        assert!(metrics.contains("openharness_telemetry_export_failures_total"));
        Ok(())
    }
}

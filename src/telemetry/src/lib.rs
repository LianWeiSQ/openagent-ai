//! Vendor-neutral telemetry contracts and OpenTelemetry runtime support.
//!
//! The durable session ledger remains the source of truth. This crate owns the
//! operational projection used for distributed tracing, metrics, log
//! correlation, propagation, redaction, and version comparison.

mod contract;
mod metrics;
mod propagation;
mod redaction;
mod runtime;

pub use contract::{
    AgentIdentity, ContractError, ExecutionState, ModelIdentity, OutcomeReason, RunOutcome,
    RunSurface, RuntimeBudgets, TaskContractV1, VersionIdentity, canonical_json_fingerprint,
};
pub use metrics::{
    FORBIDDEN_METRIC_LABELS, MetricDimensions, MetricError, OpenHarnessMetrics,
    validate_metric_label_key,
};
pub use propagation::{
    LogCorrelation, TRACEPARENT_HEADER, TRACESTATE_HEADER, TraceContext, TraceContextError,
};
pub use redaction::{
    ATTRIBUTE_VALUE_MAX_CHARS, AttributeValue, REDACTED_VALUE, SENSITIVE_ATTRIBUTE_FRAGMENTS,
    TelemetryAttributes, is_sensitive_attribute_key,
};
pub use runtime::{
    OpenHarnessSpan, SpanKind, TelemetryConfig, TelemetryHealth, TelemetryRuntime,
    TelemetryRuntimeError,
};

/// Current schema version for the telemetry contract.
pub const TELEMETRY_SCHEMA_VERSION: &str = "openharness.telemetry.v1";

/// Instrumentation scope used for traces and metrics.
pub const INSTRUMENTATION_SCOPE: &str = "openagent.telemetry";

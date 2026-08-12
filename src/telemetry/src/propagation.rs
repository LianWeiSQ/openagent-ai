use std::{collections::BTreeMap, error::Error, fmt, str::FromStr};

use opentelemetry::{
    Context,
    trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState},
};
use opentelemetry_sdk::trace::{IdGenerator, RandomIdGenerator};
use serde::{Deserialize, Serialize};

pub const TRACEPARENT_HEADER: &str = "traceparent";
pub const TRACESTATE_HEADER: &str = "tracestate";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub trace_flags: u8,
    pub trace_state: Option<String>,
    pub remote: bool,
}

impl TraceContext {
    #[must_use]
    pub fn new_root(sampled: bool) -> Self {
        let generator = RandomIdGenerator::default();
        Self {
            trace_id: generator.new_trace_id().to_string(),
            span_id: generator.new_span_id().to_string(),
            trace_flags: TraceFlags::NOT_SAMPLED.with_sampled(sampled).to_u8(),
            trace_state: None,
            remote: false,
        }
    }

    #[must_use]
    pub fn child(&self) -> Self {
        let generator = RandomIdGenerator::default();
        Self {
            trace_id: self.trace_id.clone(),
            span_id: generator.new_span_id().to_string(),
            trace_flags: self.trace_flags,
            trace_state: self.trace_state.clone(),
            remote: false,
        }
    }

    pub fn validate(&self) -> Result<(), TraceContextError> {
        let trace_id = parse_trace_id(&self.trace_id)?;
        let span_id = parse_span_id(&self.span_id)?;
        if trace_id == TraceId::INVALID {
            return Err(TraceContextError::InvalidTraceId);
        }
        if span_id == SpanId::INVALID {
            return Err(TraceContextError::InvalidSpanId);
        }
        if self.trace_flags & !TraceFlags::SAMPLED.to_u8() != 0 {
            return Err(TraceContextError::InvalidTraceFlags);
        }
        if let Some(value) = self.trace_state.as_deref() {
            TraceState::from_str(value).map_err(|_| TraceContextError::InvalidTraceState)?;
        }
        Ok(())
    }

    pub fn from_headers(
        headers: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, TraceContextError> {
        let traceparent = header_value(headers, TRACEPARENT_HEADER);
        let Some(traceparent) = traceparent else {
            return Ok(None);
        };
        let fields = traceparent.split('-').collect::<Vec<_>>();
        if fields.len() != 4 || fields[0] != "00" || fields[3].len() != 2 {
            return Err(TraceContextError::InvalidTraceparent);
        }
        let trace_flags =
            u8::from_str_radix(fields[3], 16).map_err(|_| TraceContextError::InvalidTraceFlags)?;
        let context = Self {
            trace_id: fields[1].to_ascii_lowercase(),
            span_id: fields[2].to_ascii_lowercase(),
            trace_flags,
            trace_state: header_value(headers, TRACESTATE_HEADER).map(ToString::to_string),
            remote: true,
        };
        context.validate()?;
        Ok(Some(context))
    }

    pub fn inject_headers(
        &self,
        headers: &mut BTreeMap<String, String>,
    ) -> Result<(), TraceContextError> {
        self.validate()?;
        headers.insert(
            TRACEPARENT_HEADER.to_string(),
            format!(
                "00-{}-{}-{:02x}",
                self.trace_id, self.span_id, self.trace_flags
            ),
        );
        if let Some(value) = self.trace_state.as_ref().filter(|value| !value.is_empty()) {
            headers.insert(TRACESTATE_HEADER.to_string(), value.clone());
        } else {
            headers.remove(TRACESTATE_HEADER);
        }
        Ok(())
    }

    pub(crate) fn to_otel_context(&self) -> Result<Context, TraceContextError> {
        self.validate()?;
        let trace_state = self
            .trace_state
            .as_deref()
            .map(TraceState::from_str)
            .transpose()
            .map_err(|_| TraceContextError::InvalidTraceState)?
            .unwrap_or_default();
        let span_context = SpanContext::new(
            parse_trace_id(&self.trace_id)?,
            parse_span_id(&self.span_id)?,
            TraceFlags::new(self.trace_flags),
            self.remote,
            trace_state,
        );
        Ok(Context::new().with_remote_span_context(span_context))
    }

    pub(crate) fn from_span_context(context: &SpanContext) -> Self {
        let trace_state = context.trace_state().header();
        Self {
            trace_id: context.trace_id().to_string(),
            span_id: context.span_id().to_string(),
            trace_flags: context.trace_flags().to_u8(),
            trace_state: (!trace_state.is_empty()).then_some(trace_state),
            remote: context.is_remote(),
        }
    }

    #[must_use]
    pub fn log_correlation(&self) -> LogCorrelation {
        LogCorrelation {
            trace_id: self.trace_id.clone(),
            span_id: self.span_id.clone(),
            trace_flags: format!("{:02x}", self.trace_flags),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LogCorrelation {
    pub trace_id: String,
    pub span_id: String,
    pub trace_flags: String,
}

impl LogCorrelation {
    #[must_use]
    pub fn fields(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("trace_id".to_string(), self.trace_id.clone()),
            ("span_id".to_string(), self.span_id.clone()),
            ("trace_flags".to_string(), self.trace_flags.clone()),
        ])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceContextError {
    InvalidTraceparent,
    InvalidTraceId,
    InvalidSpanId,
    InvalidTraceFlags,
    InvalidTraceState,
}

impl fmt::Display for TraceContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidTraceparent => "invalid W3C traceparent",
            Self::InvalidTraceId => "invalid W3C trace id",
            Self::InvalidSpanId => "invalid W3C span id",
            Self::InvalidTraceFlags => "invalid W3C trace flags",
            Self::InvalidTraceState => "invalid W3C tracestate",
        })
    }
}

impl Error for TraceContextError {}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
}

fn parse_trace_id(value: &str) -> Result<TraceId, TraceContextError> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TraceContextError::InvalidTraceId);
    }
    TraceId::from_hex(value).map_err(|_| TraceContextError::InvalidTraceId)
}

fn parse_span_id(value: &str) -> Result<SpanId, TraceContextError> {
    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TraceContextError::InvalidSpanId);
    }
    SpanId::from_hex(value).map_err(|_| TraceContextError::InvalidSpanId)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn w3c_headers_round_trip_case_insensitively() -> Result<(), TraceContextError> {
        let context = TraceContext::new_root(true);
        let mut headers = BTreeMap::new();
        context.inject_headers(&mut headers)?;
        let mixed_case = BTreeMap::from([
            (
                "TraceParent".to_string(),
                headers[TRACEPARENT_HEADER].clone(),
            ),
            ("TraceState".to_string(), "vendor=value".to_string()),
        ]);
        let extracted = TraceContext::from_headers(&mixed_case)?
            .ok_or(TraceContextError::InvalidTraceparent)?;
        assert_eq!(extracted.trace_id, context.trace_id);
        assert_eq!(extracted.span_id, context.span_id);
        assert!(extracted.remote);
        assert_eq!(extracted.trace_state.as_deref(), Some("vendor=value"));
        Ok(())
    }

    #[test]
    fn rejects_zero_and_malformed_contexts() {
        let zero = BTreeMap::from([(
            TRACEPARENT_HEADER.to_string(),
            "00-00000000000000000000000000000000-0000000000000000-01".to_string(),
        )]);
        assert!(TraceContext::from_headers(&zero).is_err());
        let malformed = BTreeMap::from([(
            TRACEPARENT_HEADER.to_string(),
            "00-short-short-01".to_string(),
        )]);
        assert!(TraceContext::from_headers(&malformed).is_err());
    }
}

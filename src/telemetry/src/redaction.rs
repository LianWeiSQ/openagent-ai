use std::collections::BTreeMap;

use opentelemetry::KeyValue;
use serde::{Deserialize, Serialize};

pub const ATTRIBUTE_VALUE_MAX_CHARS: usize = 256;
pub const REDACTED_VALUE: &str = "[REDACTED]";
pub const SENSITIVE_ATTRIBUTE_FRAGMENTS: &[&str] = &[
    "api_key",
    "authorization",
    "content",
    "cookie",
    "credential",
    "input",
    "message",
    "output",
    "password",
    "prompt",
    "secret",
    "token_value",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

impl AttributeValue {
    fn into_key_value(self, key: String) -> KeyValue {
        match self {
            Self::String(value) => KeyValue::new(key, value),
            Self::Integer(value) => KeyValue::new(key, value),
            Self::Float(value) => KeyValue::new(key, value),
            Self::Boolean(value) => KeyValue::new(key, value),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TelemetryAttributes {
    values: BTreeMap<String, AttributeValue>,
}

impl TelemetryAttributes {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn insert(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.insert_mut(key, AttributeValue::String(value.into()));
        self
    }

    #[must_use]
    pub fn insert_i64(mut self, key: impl Into<String>, value: i64) -> Self {
        self.insert_mut(key, AttributeValue::Integer(value));
        self
    }

    #[must_use]
    pub fn insert_f64(mut self, key: impl Into<String>, value: f64) -> Self {
        self.insert_mut(key, AttributeValue::Float(value));
        self
    }

    #[must_use]
    pub fn insert_bool(mut self, key: impl Into<String>, value: bool) -> Self {
        self.insert_mut(key, AttributeValue::Boolean(value));
        self
    }

    pub fn extend_strings(&mut self, values: BTreeMap<String, String>) {
        for (key, value) in values {
            self.insert_mut(key, AttributeValue::String(value));
        }
    }

    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, AttributeValue> {
        &self.values
    }

    pub(crate) fn into_key_values(self) -> Vec<KeyValue> {
        self.values
            .into_iter()
            .map(|(key, value)| value.into_key_value(key))
            .collect()
    }

    fn insert_mut(&mut self, key: impl Into<String>, value: AttributeValue) {
        let key = normalize_key(key.into());
        if key.is_empty() {
            return;
        }
        let value = if is_sensitive_attribute_key(&key) {
            AttributeValue::String(REDACTED_VALUE.to_string())
        } else {
            sanitize_value(value)
        };
        self.values.insert(key, value);
    }
}

#[must_use]
pub fn is_sensitive_attribute_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    SENSITIVE_ATTRIBUTE_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
}

fn normalize_key(key: String) -> String {
    key.trim()
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn sanitize_value(value: AttributeValue) -> AttributeValue {
    match value {
        AttributeValue::String(value) => AttributeValue::String(
            value
                .chars()
                .take(ATTRIBUTE_VALUE_MAX_CHARS)
                .collect::<String>(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_fields_are_redacted_and_values_are_bounded() {
        let attributes = TelemetryAttributes::new()
            .insert("gen_ai.prompt", "do not export")
            .insert("safe.field", "x".repeat(ATTRIBUTE_VALUE_MAX_CHARS + 10));
        assert_eq!(
            attributes.values().get("gen_ai.prompt"),
            Some(&AttributeValue::String(REDACTED_VALUE.to_string()))
        );
        let safe_length = attributes
            .values()
            .get("safe.field")
            .and_then(|value| match value {
                AttributeValue::String(value) => Some(value.chars().count()),
                _ => None,
            });
        assert_eq!(safe_length, Some(ATTRIBUTE_VALUE_MAX_CHARS));
    }
}

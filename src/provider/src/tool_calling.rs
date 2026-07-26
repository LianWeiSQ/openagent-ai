use std::{collections::BTreeMap, str::FromStr};

use openagent_protocol::{ToolCall, ToolCallPolicy, ToolChoice, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::ProviderStreamEvent;

pub const TOOL_CALL_DIALECT_OPTION: &str = "tool_call_dialect";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallDialect {
    OpenAiChat,
    OpenAiResponses,
    Anthropic,
    Gemini,
    Hermes,
    QwenXml,
    DeepSeek,
    Pythonic,
}

impl ToolCallDialect {
    #[must_use]
    pub const fn is_native(self) -> bool {
        matches!(
            self,
            Self::OpenAiChat | Self::OpenAiResponses | Self::Anthropic | Self::Gemini
        )
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat",
            Self::OpenAiResponses => "openai_responses",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Hermes => "hermes",
            Self::QwenXml => "qwen_xml",
            Self::DeepSeek => "deepseek",
            Self::Pythonic => "pythonic",
        }
    }
}

impl FromStr for ToolCallDialect {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "openai_chat" | "chat" | "chat_completions" => Ok(Self::OpenAiChat),
            "openai_responses" | "responses" => Ok(Self::OpenAiResponses),
            "anthropic" | "anthropic_messages" | "messages" => Ok(Self::Anthropic),
            "gemini" | "google" | "generate_content" => Ok(Self::Gemini),
            "hermes" | "hermes_xml" => Ok(Self::Hermes),
            "qwen" | "qwen_xml" => Ok(Self::QwenXml),
            "deepseek" | "deepseek_tokens" => Ok(Self::DeepSeek),
            "pythonic" | "python" | "function_call" => Ok(Self::Pythonic),
            _ => Err(format!("unsupported tool call dialect: {value}")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallArgumentsFrame {
    Delta { text: String },
    Snapshot { text: String },
    Structured { value: Value },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallFrame {
    Start {
        stream_id: String,
        call_id: Option<String>,
        name: Option<String>,
    },
    Arguments {
        stream_id: String,
        arguments: ToolCallArgumentsFrame,
    },
    End {
        stream_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolCallAssemblyError {
    pub code: Box<str>,
    pub dialect: ToolCallDialect,
    pub stream_id: Box<str>,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub message: Box<str>,
    pub raw_arguments: Option<Box<str>>,
}

impl ToolCallAssemblyError {
    fn new(
        code: impl Into<String>,
        dialect: ToolCallDialect,
        stream_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into().into_boxed_str(),
            dialect,
            stream_id: stream_id.into().into_boxed_str(),
            call_id: None,
            name: None,
            message: message.into().into_boxed_str(),
            raw_arguments: None,
        }
    }

    fn with_pending(mut self, pending: &PendingToolCall) -> Self {
        self.call_id = pending.call_id.clone();
        self.name = pending.name.clone();
        self.raw_arguments =
            (!pending.arguments.is_empty()).then(|| pending.arguments.clone().into_boxed_str());
        self
    }
}

#[derive(Clone, Debug, Default)]
struct PendingToolCall {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    structured: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct ToolCallAssembler {
    dialect: ToolCallDialect,
    pending: BTreeMap<String, PendingToolCall>,
    order: Vec<String>,
}

impl ToolCallAssembler {
    #[must_use]
    pub fn new(dialect: ToolCallDialect) -> Self {
        Self {
            dialect,
            pending: BTreeMap::new(),
            order: Vec::new(),
        }
    }

    pub fn push(
        &mut self,
        frame: ToolCallFrame,
    ) -> Result<Option<ToolCall>, ToolCallAssemblyError> {
        match frame {
            ToolCallFrame::Start {
                stream_id,
                call_id,
                name,
            } => {
                if !self.pending.contains_key(&stream_id) {
                    self.order.push(stream_id.clone());
                }
                let pending = self.pending.entry(stream_id.clone()).or_default();
                merge_identity(
                    self.dialect,
                    &stream_id,
                    "call id",
                    &mut pending.call_id,
                    call_id,
                )?;
                merge_identity(
                    self.dialect,
                    &stream_id,
                    "tool name",
                    &mut pending.name,
                    name,
                )?;
                Ok(None)
            }
            ToolCallFrame::Arguments {
                stream_id,
                arguments,
            } => {
                let Some(pending) = self.pending.get_mut(&stream_id) else {
                    return Err(ToolCallAssemblyError::new(
                        "tool_call_missing_start",
                        self.dialect,
                        stream_id,
                        "tool arguments arrived before the tool call start frame",
                    ));
                };
                match arguments {
                    ToolCallArgumentsFrame::Delta { text } => {
                        if pending.structured.is_some() {
                            return Err(ToolCallAssemblyError::new(
                                "tool_call_mixed_arguments",
                                self.dialect,
                                stream_id,
                                "tool call mixed structured arguments with text deltas",
                            )
                            .with_pending(pending));
                        }
                        pending.arguments.push_str(&text);
                    }
                    ToolCallArgumentsFrame::Snapshot { text } => {
                        if pending.structured.is_some() {
                            return Err(ToolCallAssemblyError::new(
                                "tool_call_mixed_arguments",
                                self.dialect,
                                stream_id,
                                "tool call mixed structured arguments with a text snapshot",
                            )
                            .with_pending(pending));
                        }
                        pending.arguments = text;
                    }
                    ToolCallArgumentsFrame::Structured { value } => {
                        if !pending.arguments.is_empty() {
                            return Err(ToolCallAssemblyError::new(
                                "tool_call_mixed_arguments",
                                self.dialect,
                                stream_id,
                                "tool call mixed text arguments with a structured value",
                            )
                            .with_pending(pending));
                        }
                        pending.structured = Some(value);
                    }
                }
                Ok(None)
            }
            ToolCallFrame::End { stream_id } => self.finish_one(&stream_id).map(Some),
        }
    }

    pub fn finish_all(&mut self) -> Vec<Result<ToolCall, ToolCallAssemblyError>> {
        let order = std::mem::take(&mut self.order);
        let mut completed = Vec::new();
        for stream_id in order {
            if self.pending.contains_key(&stream_id) {
                completed.push(self.finish_one(&stream_id));
            }
        }
        completed
    }

    pub fn abort_incomplete(&mut self) -> Vec<ToolCallAssemblyError> {
        let order = std::mem::take(&mut self.order);
        order
            .into_iter()
            .filter_map(|stream_id| {
                self.pending.remove(&stream_id).map(|pending| {
                    ToolCallAssemblyError::new(
                        "tool_call_truncated",
                        self.dialect,
                        stream_id,
                        "provider stream ended before the tool call completed",
                    )
                    .with_pending(&pending)
                })
            })
            .collect()
    }

    fn finish_one(&mut self, stream_id: &str) -> Result<ToolCall, ToolCallAssemblyError> {
        let Some(pending) = self.pending.remove(stream_id) else {
            return Err(ToolCallAssemblyError::new(
                "tool_call_missing_start",
                self.dialect,
                stream_id,
                "tool call end frame had no matching start frame",
            ));
        };
        self.order.retain(|candidate| candidate != stream_id);
        let name = pending
            .name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ToolCallAssemblyError::new(
                    "tool_call_missing_name",
                    self.dialect,
                    stream_id,
                    "tool call completed without a tool name",
                )
                .with_pending(&pending)
            })?;
        let input = parse_complete_arguments(self.dialect, stream_id, &pending)?;
        let call_id = pending
            .call_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("{}_call_{stream_id}", self.dialect.as_str()));
        Ok(ToolCall {
            name,
            input,
            call_id,
        })
    }
}

fn merge_identity(
    dialect: ToolCallDialect,
    stream_id: &str,
    label: &str,
    current: &mut Option<String>,
    incoming: Option<String>,
) -> Result<(), ToolCallAssemblyError> {
    let Some(incoming) = incoming.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    if let Some(current) = current {
        if current != &incoming {
            return Err(ToolCallAssemblyError::new(
                "tool_call_identity_changed",
                dialect,
                stream_id,
                format!("{label} changed within one streamed tool call"),
            ));
        }
    } else {
        *current = Some(incoming);
    }
    Ok(())
}

fn parse_complete_arguments(
    dialect: ToolCallDialect,
    stream_id: &str,
    pending: &PendingToolCall,
) -> Result<Value, ToolCallAssemblyError> {
    if let Some(value) = &pending.structured {
        return match value {
            Value::Object(_) => Ok(value.clone()),
            Value::Null => Ok(json!({})),
            _ => Err(ToolCallAssemblyError::new(
                "tool_call_arguments_not_object",
                dialect,
                stream_id,
                "tool call arguments must be a JSON object",
            )
            .with_pending(pending)),
        };
    }
    let raw = pending.arguments.trim();
    if raw.is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(arguments)) => Ok(Value::Object(arguments)),
        Ok(_) => Err(ToolCallAssemblyError::new(
            "tool_call_arguments_not_object",
            dialect,
            stream_id,
            "tool call arguments must be a JSON object",
        )
        .with_pending(pending)),
        Err(error) => Err(ToolCallAssemblyError::new(
            "tool_call_invalid_json",
            dialect,
            stream_id,
            format!("tool call arguments are not valid JSON: {error}"),
        )
        .with_pending(pending)),
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ParsedTextToolCalls {
    pub calls: Vec<ToolCall>,
    pub remaining_text: String,
}

pub fn parse_text_tool_calls(
    dialect: ToolCallDialect,
    text: &str,
) -> Result<ParsedTextToolCalls, Vec<ToolCallAssemblyError>> {
    match dialect {
        ToolCallDialect::Hermes => parse_json_blocks(dialect, text, "<tool_call>", "</tool_call>"),
        ToolCallDialect::QwenXml => parse_qwen_xml(text),
        ToolCallDialect::DeepSeek => parse_deepseek(text),
        ToolCallDialect::Pythonic => parse_pythonic(text),
        _ => Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        }),
    }
}

pub fn apply_tool_call_dialect(
    events: Vec<ProviderStreamEvent>,
    dialect: ToolCallDialect,
) -> Vec<ProviderStreamEvent> {
    if dialect.is_native() {
        return events;
    }
    if events
        .iter()
        .any(|event| matches!(event, ProviderStreamEvent::ToolCall { .. }))
    {
        return events;
    }
    let text = events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let parsed = parse_text_tool_calls(dialect, &text);
    let mut output = events
        .into_iter()
        .filter(|event| !matches!(event, ProviderStreamEvent::TextDelta { .. }))
        .collect::<Vec<_>>();
    let finish_index = output
        .iter()
        .position(|event| matches!(event, ProviderStreamEvent::Finish { .. }))
        .unwrap_or(output.len());
    match parsed {
        Ok(parsed) => {
            let has_tool_calls = !parsed.calls.is_empty();
            if !parsed.remaining_text.is_empty() {
                output.insert(
                    finish_index,
                    ProviderStreamEvent::TextDelta {
                        text: parsed.remaining_text,
                    },
                );
            }
            let mut insert_at = output
                .iter()
                .position(|event| matches!(event, ProviderStreamEvent::Finish { .. }))
                .unwrap_or(output.len());
            for call in parsed.calls {
                output.insert(
                    insert_at,
                    ProviderStreamEvent::ToolCall {
                        call_id: call.call_id,
                        name: call.name,
                        input: call.input,
                    },
                );
                insert_at += 1;
            }
            if has_tool_calls
                && let Some(ProviderStreamEvent::Finish { finish_reason, .. }) =
                    output.get_mut(insert_at)
            {
                *finish_reason = "tool_call".to_string();
            }
        }
        Err(errors) => {
            for (offset, error) in errors.into_iter().enumerate() {
                output.insert(
                    finish_index + offset,
                    ProviderStreamEvent::ToolCallError { error },
                );
            }
        }
    }
    output
}

fn parse_json_blocks(
    dialect: ToolCallDialect,
    text: &str,
    start: &str,
    end: &str,
) -> Result<ParsedTextToolCalls, Vec<ToolCallAssemblyError>> {
    let blocks = extract_blocks(text, start, end);
    if blocks.is_empty() {
        if text.contains(start) {
            return Err(vec![ToolCallAssemblyError::new(
                "tool_call_truncated",
                dialect,
                "0",
                format!("missing closing {end} marker"),
            )]);
        }
        return Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        });
    }
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    for (index, (_, _, raw)) in blocks.iter().enumerate() {
        match parse_text_tool_object(dialect, index, raw) {
            Ok(call) => calls.push(call),
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ParsedTextToolCalls {
        calls,
        remaining_text: remove_ranges(text, &blocks),
    })
}

fn parse_text_tool_object(
    dialect: ToolCallDialect,
    index: usize,
    raw: &str,
) -> Result<ToolCall, ToolCallAssemblyError> {
    let stream_id = index.to_string();
    let value = serde_json::from_str::<Value>(strip_code_fence(raw).trim()).map_err(|error| {
        let mut assembly_error = ToolCallAssemblyError::new(
            "tool_call_invalid_json",
            dialect,
            &stream_id,
            format!("text tool call is not valid JSON: {error}"),
        );
        assembly_error.raw_arguments = Some(raw.trim().into());
        assembly_error
    })?;
    let object = value.as_object().ok_or_else(|| {
        ToolCallAssemblyError::new(
            "tool_call_invalid_shape",
            dialect,
            &stream_id,
            "text tool call must be a JSON object",
        )
    })?;
    let function = object.get("function").and_then(Value::as_object);
    let name = object
        .get("name")
        .or_else(|| object.get("tool"))
        .and_then(Value::as_str)
        .or_else(|| {
            function
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ToolCallAssemblyError::new(
                "tool_call_missing_name",
                dialect,
                &stream_id,
                "text tool call did not include a tool name",
            )
        })?;
    let arguments = object
        .get("arguments")
        .or_else(|| object.get("parameters"))
        .or_else(|| object.get("input"))
        .or_else(|| function.and_then(|function| function.get("arguments")))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let arguments = match arguments {
        Value::String(raw) => serde_json::from_str::<Value>(&raw).map_err(|error| {
            let mut assembly_error = ToolCallAssemblyError::new(
                "tool_call_invalid_json",
                dialect,
                &stream_id,
                format!("text tool call arguments are not valid JSON: {error}"),
            );
            assembly_error.name = Some(name.to_string());
            assembly_error.raw_arguments = Some(raw.into_boxed_str());
            assembly_error
        })?,
        value => value,
    };
    if !arguments.is_object() {
        return Err(ToolCallAssemblyError::new(
            "tool_call_arguments_not_object",
            dialect,
            &stream_id,
            "text tool call arguments must be a JSON object",
        ));
    }
    let call_id = object
        .get("id")
        .or_else(|| object.get("call_id"))
        .and_then(Value::as_str)
        .map_or_else(
            || format!("{}_call_{index}", dialect.as_str()),
            str::to_string,
        );
    Ok(ToolCall {
        name: name.to_string(),
        input: arguments,
        call_id,
    })
}

fn parse_qwen_xml(text: &str) -> Result<ParsedTextToolCalls, Vec<ToolCallAssemblyError>> {
    if text.contains("<tool_call>") {
        return parse_json_blocks(
            ToolCallDialect::QwenXml,
            text,
            "<tool_call>",
            "</tool_call>",
        );
    }
    let start = "<function=";
    if !text.contains(start) {
        return Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        });
    }
    let mut calls = Vec::new();
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find(start) {
        let absolute_start = cursor + relative_start;
        let Some(name_end_relative) = text[absolute_start + start.len()..].find('>') else {
            return Err(vec![ToolCallAssemblyError::new(
                "tool_call_truncated",
                ToolCallDialect::QwenXml,
                calls.len().to_string(),
                "Qwen function tag is missing `>`",
            )]);
        };
        let name_end = absolute_start + start.len() + name_end_relative;
        let name = text[absolute_start + start.len()..name_end].trim();
        let end_tag = "</function>";
        let Some(body_end_relative) = text[name_end + 1..].find(end_tag) else {
            return Err(vec![ToolCallAssemblyError::new(
                "tool_call_truncated",
                ToolCallDialect::QwenXml,
                calls.len().to_string(),
                "Qwen function tag is missing `</function>`",
            )]);
        };
        let body_end = name_end + 1 + body_end_relative;
        let body = &text[name_end + 1..body_end];
        let input = parse_qwen_parameters(body).map_err(|message| {
            vec![ToolCallAssemblyError::new(
                "tool_call_invalid_parameters",
                ToolCallDialect::QwenXml,
                calls.len().to_string(),
                message,
            )]
        })?;
        calls.push(ToolCall {
            name: name.to_string(),
            input: Value::Object(input),
            call_id: format!("qwen_xml_call_{}", calls.len()),
        });
        let range_end = body_end + end_tag.len();
        ranges.push((absolute_start, range_end, String::new()));
        cursor = range_end;
    }
    Ok(ParsedTextToolCalls {
        calls,
        remaining_text: remove_ranges(text, &ranges),
    })
}

fn parse_qwen_parameters(body: &str) -> Result<Map<String, Value>, String> {
    let mut parameters = Map::new();
    let mut cursor = 0;
    let marker = "<parameter=";
    while let Some(relative_start) = body[cursor..].find(marker) {
        let absolute_start = cursor + relative_start;
        let name_start = absolute_start + marker.len();
        let name_end = body[name_start..]
            .find('>')
            .map(|offset| name_start + offset)
            .ok_or_else(|| "Qwen parameter tag is missing `>`".to_string())?;
        let name = body[name_start..name_end].trim();
        if name.is_empty() {
            return Err("Qwen parameter name is empty".to_string());
        }
        let end_tag = "</parameter>";
        let value_end = body[name_end + 1..]
            .find(end_tag)
            .map(|offset| name_end + 1 + offset)
            .ok_or_else(|| "Qwen parameter tag is missing `</parameter>`".to_string())?;
        let raw_value = body[name_end + 1..value_end].trim();
        parameters.insert(name.to_string(), parse_scalar_value(raw_value));
        cursor = value_end + end_tag.len();
    }
    Ok(parameters)
}

fn parse_deepseek(text: &str) -> Result<ParsedTextToolCalls, Vec<ToolCallAssemblyError>> {
    const START: &str = "<｜tool▁call▁begin｜>";
    const END: &str = "<｜tool▁call▁end｜>";
    const SEP: &str = "<｜tool▁sep｜>";
    let blocks = extract_blocks(text, START, END);
    if blocks.is_empty() {
        if text.contains(START) {
            return Err(vec![ToolCallAssemblyError::new(
                "tool_call_truncated",
                ToolCallDialect::DeepSeek,
                "0",
                "DeepSeek tool call is missing its end marker",
            )]);
        }
        return Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        });
    }
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    for (index, (_, _, raw)) in blocks.iter().enumerate() {
        let raw = raw.trim();
        let body = raw
            .split_once(SEP)
            .map_or(raw, |(_, arguments)| arguments)
            .trim();
        let mut lines = body.lines().map(str::trim).filter(|line| !line.is_empty());
        let name = lines.next().unwrap_or_default();
        let arguments = strip_code_fence(&lines.collect::<Vec<_>>().join("\n"))
            .trim()
            .to_string();
        let mut assembler = ToolCallAssembler::new(ToolCallDialect::DeepSeek);
        let stream_id = index.to_string();
        let result = assembler
            .push(ToolCallFrame::Start {
                stream_id: stream_id.clone(),
                call_id: Some(format!("deepseek_call_{index}")),
                name: (!name.is_empty()).then(|| name.to_string()),
            })
            .and_then(|_| {
                assembler.push(ToolCallFrame::Arguments {
                    stream_id: stream_id.clone(),
                    arguments: ToolCallArgumentsFrame::Snapshot { text: arguments },
                })
            })
            .and_then(|_| assembler.push(ToolCallFrame::End { stream_id }))
            .and_then(|call| {
                call.ok_or_else(|| {
                    ToolCallAssemblyError::new(
                        "tool_call_incomplete",
                        ToolCallDialect::DeepSeek,
                        index.to_string(),
                        "DeepSeek tool call did not complete",
                    )
                })
            });
        match result {
            Ok(call) => calls.push(call),
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ParsedTextToolCalls {
        calls,
        remaining_text: remove_ranges(text, &blocks),
    })
}

fn parse_pythonic(text: &str) -> Result<ParsedTextToolCalls, Vec<ToolCallAssemblyError>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        });
    }
    let Some(open) = trimmed.find('(') else {
        return Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        });
    };
    if !trimmed.ends_with(')') {
        return Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        });
    }
    let name = trimmed[..open].trim();
    if !valid_tool_name(name) {
        return Ok(ParsedTextToolCalls {
            calls: Vec::new(),
            remaining_text: text.to_string(),
        });
    }
    let raw_arguments = &trimmed[open + 1..trimmed.len() - 1];
    let pairs = match split_top_level(raw_arguments, ',') {
        Ok(pairs) => pairs,
        Err(message) => {
            return Err(vec![ToolCallAssemblyError::new(
                "tool_call_invalid_parameters",
                ToolCallDialect::Pythonic,
                "0",
                message,
            )]);
        }
    };
    let mut arguments = Map::new();
    for pair in pairs.into_iter().filter(|pair| !pair.trim().is_empty()) {
        let Some((key, value)) = split_assignment(&pair) else {
            return Err(vec![ToolCallAssemblyError::new(
                "tool_call_invalid_parameters",
                ToolCallDialect::Pythonic,
                "0",
                "Pythonic tool arguments must use `name=value`",
            )]);
        };
        if !valid_tool_name(key.trim()) {
            return Err(vec![ToolCallAssemblyError::new(
                "tool_call_invalid_parameters",
                ToolCallDialect::Pythonic,
                "0",
                "Pythonic tool argument name is invalid",
            )]);
        }
        arguments.insert(key.trim().to_string(), parse_scalar_value(value.trim()));
    }
    Ok(ParsedTextToolCalls {
        calls: vec![ToolCall {
            name: name.to_string(),
            input: Value::Object(arguments),
            call_id: "pythonic_call_0".to_string(),
        }],
        remaining_text: String::new(),
    })
}

fn extract_blocks<'a>(text: &'a str, start: &str, end: &str) -> Vec<(usize, usize, &'a str)> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find(start) {
        let absolute_start = cursor + relative_start;
        let content_start = absolute_start + start.len();
        let Some(relative_end) = text[content_start..].find(end) else {
            break;
        };
        let content_end = content_start + relative_end;
        let absolute_end = content_end + end.len();
        blocks.push((
            absolute_start,
            absolute_end,
            &text[content_start..content_end],
        ));
        cursor = absolute_end;
    }
    blocks
}

fn remove_ranges(text: &str, ranges: &[(usize, usize, impl AsRef<str>)]) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    for (start, end, _) in ranges {
        if *start >= cursor && *end <= text.len() {
            output.push_str(&text[cursor..*start]);
            cursor = *end;
        }
    }
    output.push_str(&text[cursor..]);
    output.trim().to_string()
}

fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let after_language = after_open
        .find('\n')
        .map_or(after_open, |newline| &after_open[newline + 1..]);
    after_language
        .strip_suffix("```")
        .unwrap_or(after_language)
        .trim()
}

fn valid_tool_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.'))
}

fn split_assignment(value: &str) -> Option<(&str, &str)> {
    let mut quote = None;
    let mut depth = 0_i64;
    for (index, character) in value.char_indices() {
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if quote.is_some() {
            continue;
        }
        match character {
            '[' | '{' | '(' => depth += 1,
            ']' | '}' | ')' => depth -= 1,
            '=' if depth == 0 => return Some((&value[..index], &value[index + 1..])),
            _ => {}
        }
    }
    None
}

fn split_top_level(value: &str, delimiter: char) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0_i64;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            current.push(character);
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            current.push(character);
            continue;
        }
        if quote.is_none() {
            match character {
                '[' | '{' | '(' => depth += 1,
                ']' | '}' | ')' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                return Err("unbalanced Pythonic tool arguments".to_string());
            }
            if character == delimiter && depth == 0 {
                parts.push(current.trim().to_string());
                current.clear();
                continue;
            }
        }
        current.push(character);
    }
    if quote.is_some() || depth != 0 {
        return Err("unterminated Pythonic tool argument".to_string());
    }
    parts.push(current.trim().to_string());
    Ok(parts)
}

fn parse_scalar_value(raw: &str) -> Value {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return value;
    }
    if raw.eq_ignore_ascii_case("none") {
        return Value::Null;
    }
    if raw.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
        return Value::String(
            raw[1..raw.len() - 1]
                .replace("\\'", "'")
                .replace("\\\\", "\\"),
        );
    }
    Value::String(raw.to_string())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapability {
    NativeToolCalls,
    StreamingToolCalls,
    ParallelToolCalls,
    StrictToolSchemas,
    ToolOutputSchemas,
    ToolChoiceAuto,
    ToolChoiceNone,
    ToolChoiceRequired,
    ToolChoiceNamed,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCapabilitySet {
    pub values: std::collections::BTreeSet<ProviderCapability>,
}

impl ProviderCapabilitySet {
    #[must_use]
    pub fn new(values: impl IntoIterator<Item = ProviderCapability>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn supports(&self, capability: ProviderCapability) -> bool {
        self.values.contains(&capability)
    }

    #[must_use]
    pub fn intersection(&self, requested: &Self) -> Self {
        Self {
            values: self
                .values
                .intersection(&requested.values)
                .copied()
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NegotiatedToolCallPolicy {
    pub requested: ToolCallPolicy,
    pub effective: ToolCallPolicy,
    pub capabilities: ProviderCapabilitySet,
    pub strict_tools: Vec<String>,
    pub output_schema_tools: Vec<String>,
}

pub fn provider_capabilities(provider: &str, dialect: ToolCallDialect) -> ProviderCapabilitySet {
    use ProviderCapability::{
        NativeToolCalls, ParallelToolCalls, StreamingToolCalls, StrictToolSchemas, ToolChoiceAuto,
        ToolChoiceNamed, ToolChoiceNone, ToolChoiceRequired, ToolOutputSchemas,
    };
    let mut values = vec![
        ToolChoiceAuto,
        ToolChoiceNone,
        ToolChoiceRequired,
        ToolChoiceNamed,
    ];
    if dialect.is_native() {
        values.extend([NativeToolCalls, StreamingToolCalls]);
    }
    match dialect {
        ToolCallDialect::OpenAiChat | ToolCallDialect::OpenAiResponses => {
            values.push(ParallelToolCalls);
            if matches!(provider, "openai" | "azure-openai") {
                values.push(StrictToolSchemas);
            }
        }
        ToolCallDialect::Anthropic => {
            values.push(ParallelToolCalls);
        }
        ToolCallDialect::Gemini
        | ToolCallDialect::Hermes
        | ToolCallDialect::QwenXml
        | ToolCallDialect::DeepSeek
        | ToolCallDialect::Pythonic => {}
    }
    if provider == "ollama" {
        values.retain(|capability| {
            !matches!(
                capability,
                StrictToolSchemas | ToolOutputSchemas | ParallelToolCalls
            )
        });
    }
    ProviderCapabilitySet::new(values)
}

pub fn negotiate_tool_call_policy(
    requested: ToolCallPolicy,
    capabilities: ProviderCapabilitySet,
    tools: &[ToolSchema],
) -> Result<NegotiatedToolCallPolicy, String> {
    let choice_capability = match &requested.choice {
        ToolChoice::Auto => ProviderCapability::ToolChoiceAuto,
        ToolChoice::None => ProviderCapability::ToolChoiceNone,
        ToolChoice::Required => ProviderCapability::ToolChoiceRequired,
        ToolChoice::Tool { name } => {
            if !tools.iter().any(|tool| tool.name == *name) {
                return Err(format!("tool choice references unknown tool `{name}`"));
            }
            ProviderCapability::ToolChoiceNamed
        }
    };
    if !capabilities.supports(choice_capability) {
        return Err(format!(
            "provider does not support requested tool choice `{}`",
            tool_choice_name(&requested.choice)
        ));
    }
    let mut effective = requested.clone();
    if capabilities.supports(ProviderCapability::ParallelToolCalls) {
        if tools.iter().any(|tool| !tool.parallel_safe) {
            effective.parallel = Some(false);
        }
    } else {
        effective.parallel = None;
    }
    let strict_tools = if capabilities.supports(ProviderCapability::StrictToolSchemas) {
        tools
            .iter()
            .filter(|tool| tool.strict)
            .map(|tool| tool.name.clone())
            .collect()
    } else {
        Vec::new()
    };
    let output_schema_tools = if capabilities.supports(ProviderCapability::ToolOutputSchemas) {
        tools
            .iter()
            .filter(|tool| tool.output_schema.is_some())
            .map(|tool| tool.name.clone())
            .collect()
    } else {
        Vec::new()
    };
    Ok(NegotiatedToolCallPolicy {
        requested,
        effective,
        capabilities,
        strict_tools,
        output_schema_tools,
    })
}

#[must_use]
pub fn tool_call_policy_from_options(options: &BTreeMap<String, Value>) -> ToolCallPolicy {
    let choice = options
        .get("tool_choice")
        .and_then(parse_tool_choice)
        .unwrap_or_default();
    let parallel = options.get("parallel_tool_calls").and_then(Value::as_bool);
    ToolCallPolicy { choice, parallel }
}

pub fn tool_call_dialect_from_options(
    provider: &str,
    wire_api: &str,
    options: &BTreeMap<String, Value>,
) -> Result<ToolCallDialect, String> {
    if let Some(value) = options
        .get(TOOL_CALL_DIALECT_OPTION)
        .and_then(Value::as_str)
    {
        if value.trim().eq_ignore_ascii_case("native") {
            return native_tool_call_dialect(provider, wire_api);
        }
        return ToolCallDialect::from_str(value);
    }
    native_tool_call_dialect(provider, wire_api)
}

fn native_tool_call_dialect(provider: &str, wire_api: &str) -> Result<ToolCallDialect, String> {
    if provider == "anthropic" {
        Ok(ToolCallDialect::Anthropic)
    } else if matches!(provider, "gemini" | "google") {
        Ok(ToolCallDialect::Gemini)
    } else if wire_api == "chat" {
        Ok(ToolCallDialect::OpenAiChat)
    } else {
        Ok(ToolCallDialect::OpenAiResponses)
    }
}

fn parse_tool_choice(value: &Value) -> Option<ToolChoice> {
    if let Some(value) = value.as_str() {
        return match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None),
            "required" | "any" => Some(ToolChoice::Required),
            name if !name.is_empty() => Some(ToolChoice::Tool {
                name: name.to_string(),
            }),
            _ => None,
        };
    }
    let object = value.as_object()?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "auto" => Some(ToolChoice::Auto),
        "none" => Some(ToolChoice::None),
        "required" | "any" => Some(ToolChoice::Required),
        "tool" | "function" => object
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| {
                object
                    .get("function")
                    .and_then(Value::as_object)
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
            })
            .map(|name| ToolChoice::Tool {
                name: name.to_string(),
            }),
        _ => None,
    }
}

fn tool_choice_name(choice: &ToolChoice) -> &str {
    match choice {
        ToolChoice::Auto => "auto",
        ToolChoice::None => "none",
        ToolChoice::Required => "required",
        ToolChoice::Tool { .. } => "tool",
    }
}

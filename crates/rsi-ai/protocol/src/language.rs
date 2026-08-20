use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    AiError, MAX_CONTENT_BLOCKS, MAX_LANGUAGE_OUTPUT_BYTES, MAX_SOURCES, MAX_WARNINGS, validation,
};

const MAX_SOURCE_FIELD_BYTES: usize = 16 * 1024;
const MAX_WARNING_MESSAGE_BYTES: usize = 4 * 1024;

/// Token counts reported for one provider attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

/// Why a language response stopped.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    ContentFilter,
    Cancelled,
}

/// One model-requested tool call. Arguments deliberately remain raw text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// One complete provider-neutral content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Text { text: String },
    Reasoning { text: String },
    ToolCall(ToolCall),
}

/// Metadata that opens one streamed content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentStart {
    Text,
    Reasoning,
    ToolCall { id: String, name: String },
}

/// One incremental update for an open content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ContentDelta {
    Text(String),
    Reasoning(String),
    ToolArguments(String),
}

/// One citeable source returned by a provider-hosted operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub title: Option<String>,
    pub url: Option<String>,
}

/// A safe provider or compatibility warning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

/// Bounded provider-private JSON that never contains binary data or secrets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExtension {
    pub namespace: String,
    pub version: u32,
    pub value: Value,
}

impl ProviderExtension {
    pub(crate) fn validate(&self, field: &str) -> Result<(), StreamError> {
        validation::identifier(&format!("{field}.namespace"), &self.namespace)
            .map_err(|message| StreamError::invalid("stream.invalid_extension", message))?;
        validation::validate_json(&self.value)
            .map_err(|message| StreamError::invalid("stream.invalid_extension", message))?;
        validation::extension_size(&format!("{field}.value"), &self.value)
            .map_err(|message| StreamError::invalid("stream.extension_too_large", message))
    }
}

/// Normalized language events emitted by every adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LanguageEvent {
    ContentStarted {
        index: u32,
        content: ContentStart,
    },
    ContentDelta {
        index: u32,
        delta: ContentDelta,
    },
    ContentFinished {
        index: u32,
    },
    Source {
        source: Source,
    },
    Warning {
        warning: Warning,
    },
    Usage {
        usage: TokenUsage,
    },
    Finished {
        reason: FinishReason,
        replay: Option<ProviderExtension>,
    },
    Failed {
        error: AiError,
        replay: Option<ProviderExtension>,
    },
}

/// Complete language output assembled through the same grammar callers stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageOutput {
    pub content: Vec<ContentBlock>,
    pub finish_reason: FinishReason,
    pub usage: Option<TokenUsage>,
    pub replay: Option<ProviderExtension>,
    pub warnings: Vec<Warning>,
    pub sources: Vec<Source>,
}

impl LanguageOutput {
    /// Concatenates user-visible text blocks without including reasoning.
    #[must_use]
    pub fn visible_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Reasoning { .. } | ContentBlock::ToolCall(_) => None,
            })
            .collect()
    }

    /// Revalidates a complete output through the canonical stream grammar.
    pub fn validate(&self) -> Result<(), StreamError> {
        let mut assembler = LanguageAssembler::new();
        for (index, block) in self.content.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| {
                StreamError::invalid("stream.too_many_blocks", "content block index overflowed")
            })?;
            let (content, delta) = match block {
                ContentBlock::Text { text } => {
                    (ContentStart::Text, ContentDelta::Text(text.clone()))
                }
                ContentBlock::Reasoning { text } => (
                    ContentStart::Reasoning,
                    ContentDelta::Reasoning(text.clone()),
                ),
                ContentBlock::ToolCall(call) => (
                    ContentStart::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    },
                    ContentDelta::ToolArguments(call.arguments.clone()),
                ),
            };
            assembler.push(&LanguageEvent::ContentStarted { index, content })?;
            assembler.push(&LanguageEvent::ContentDelta { index, delta })?;
            assembler.push(&LanguageEvent::ContentFinished { index })?;
        }
        for source in &self.sources {
            assembler.push(&LanguageEvent::Source {
                source: source.clone(),
            })?;
        }
        for warning in &self.warnings {
            assembler.push(&LanguageEvent::Warning {
                warning: warning.clone(),
            })?;
        }
        if let Some(usage) = self.usage {
            assembler.push(&LanguageEvent::Usage { usage })?;
        }
        assembler.push(&LanguageEvent::Finished {
            reason: self.finish_reason.clone(),
            replay: self.replay.clone(),
        })?;
        let rebuilt = assembler.finish().map_err(|error| match error {
            LanguageAssemblyError::Protocol(error) => error,
            LanguageAssemblyError::Provider { .. } => unreachable!("success event cannot fail"),
        })?;
        if &rebuilt != self {
            return Err(StreamError::invalid(
                "stream.non_canonical_output",
                "language output is not canonical",
            ));
        }
        Ok(())
    }
}

/// Validated language content retained when a provider attempt fails.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguagePartialOutput {
    pub content: Vec<ContentBlock>,
    pub usage: Option<TokenUsage>,
    pub replay: Option<ProviderExtension>,
    pub warnings: Vec<Warning>,
    pub sources: Vec<Source>,
}

/// A stable stream-grammar failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct StreamError {
    code: &'static str,
    message: String,
}

impl StreamError {
    /// Constructs a stable stream failure at an adapter or orchestration boundary.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message)
    }

    /// Stable machine-readable failure code.
    pub const fn code(&self) -> &'static str {
        self.code
    }
}

/// Terminal language assembly failure, with provider partial output preserved.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum LanguageAssemblyError {
    #[error(transparent)]
    Protocol(#[from] StreamError),
    #[error("{error}")]
    Provider {
        error: AiError,
        partial: Box<LanguagePartialOutput>,
    },
}

impl LanguageAssemblyError {
    /// Stable code for protocol failures, or a provider-neutral error kind.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Protocol(error) => error.code(),
            Self::Provider { error, .. } => error.kind().code(),
        }
    }
}

#[derive(Debug)]
enum OpenContent {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
}

impl OpenContent {
    fn push(&mut self, delta: &ContentDelta) -> Result<usize, StreamError> {
        let added = match (self, delta) {
            (Self::Text(text), ContentDelta::Text(delta))
            | (Self::Reasoning(text), ContentDelta::Reasoning(delta)) => {
                let added = delta.len();
                text.push_str(delta);
                added
            }
            (Self::ToolCall { arguments, .. }, ContentDelta::ToolArguments(delta)) => {
                let added = delta.len();
                arguments.push_str(delta);
                added
            }
            _ => {
                return Err(StreamError::invalid(
                    "stream.content_type_mismatch",
                    "content delta does not match the open block type",
                ));
            }
        };
        Ok(added)
    }

    fn close(self) -> ContentBlock {
        match self {
            Self::Text(text) => ContentBlock::Text { text },
            Self::Reasoning(text) => ContentBlock::Reasoning { text },
            Self::ToolCall {
                id,
                name,
                arguments,
            } => ContentBlock::ToolCall(ToolCall {
                id,
                name,
                arguments,
            }),
        }
    }
}

/// Strict incremental assembler shared by direct SDK and durable consumers.
#[derive(Debug, Default)]
pub struct LanguageAssembler {
    next_index: u32,
    open: BTreeMap<u32, OpenContent>,
    content: BTreeMap<u32, ContentBlock>,
    assembled_bytes: usize,
    usage: Option<TokenUsage>,
    replay: Option<ProviderExtension>,
    finish_reason: Option<FinishReason>,
    failure: Option<AiError>,
    warnings: Vec<Warning>,
    sources: Vec<Source>,
}

impl LanguageAssembler {
    /// Starts an empty assembler.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_index: 0,
            open: BTreeMap::new(),
            content: BTreeMap::new(),
            assembled_bytes: 0,
            usage: None,
            replay: None,
            finish_reason: None,
            failure: None,
            warnings: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// Applies one normalized event while enforcing ordering and bounds.
    #[allow(clippy::too_many_lines)] // One exhaustive transition owns the stream grammar.
    pub fn push(&mut self, event: &LanguageEvent) -> Result<(), StreamError> {
        if self.finish_reason.is_some() || self.failure.is_some() {
            return Err(StreamError::invalid(
                "stream.already_finished",
                "language stream emitted an event after its terminal event",
            ));
        }

        match event {
            LanguageEvent::ContentStarted { index, content } => {
                if *index != self.next_index {
                    return Err(StreamError::invalid(
                        "stream.non_contiguous_index",
                        format!(
                            "content index {index} is not the next contiguous index {}",
                            self.next_index
                        ),
                    ));
                }
                if usize::try_from(*index).map_or(true, |index| index >= MAX_CONTENT_BLOCKS) {
                    return Err(StreamError::invalid(
                        "stream.too_many_blocks",
                        format!("language stream exceeds {MAX_CONTENT_BLOCKS} content blocks"),
                    ));
                }
                let open = match content {
                    ContentStart::Text => OpenContent::Text(String::new()),
                    ContentStart::Reasoning => OpenContent::Reasoning(String::new()),
                    ContentStart::ToolCall { id, name } => {
                        validation::identifier("tool_call.id", id).map_err(|message| {
                            StreamError::invalid("stream.invalid_tool_call", message)
                        })?;
                        validation::tool_name("tool_call.name", name).map_err(|message| {
                            StreamError::invalid("stream.invalid_tool_call", message)
                        })?;
                        self.add_bytes(id.len().saturating_add(name.len()))?;
                        OpenContent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                        }
                    }
                };
                self.open.insert(*index, open);
                self.next_index = self.next_index.saturating_add(1);
            }
            LanguageEvent::ContentDelta { index, delta } => {
                let open = self.open.get_mut(index).ok_or_else(|| {
                    StreamError::invalid(
                        "stream.content_not_started",
                        format!("content index {index} is not open"),
                    )
                })?;
                let added = open.push(delta)?;
                self.add_bytes(added)?;
            }
            LanguageEvent::ContentFinished { index } => {
                let open = self.open.remove(index).ok_or_else(|| {
                    StreamError::invalid(
                        "stream.content_not_started",
                        format!("content index {index} is not open"),
                    )
                })?;
                self.content.insert(*index, open.close());
            }
            LanguageEvent::Source { source } => {
                if self.sources.len() >= MAX_SOURCES {
                    return Err(StreamError::invalid(
                        "stream.too_many_sources",
                        format!("language stream exceeds {MAX_SOURCES} sources"),
                    ));
                }
                validation::identifier("source.id", &source.id)
                    .map_err(|message| StreamError::invalid("stream.invalid_source", message))?;
                if let Some(title) = &source.title {
                    validation::safe_text("source.title", title, MAX_SOURCE_FIELD_BYTES, false)
                        .map_err(|message| {
                            StreamError::invalid("stream.invalid_source", message)
                        })?;
                }
                if let Some(url) = &source.url {
                    validation::safe_text("source.url", url, MAX_SOURCE_FIELD_BYTES, false)
                        .map_err(|message| {
                            StreamError::invalid("stream.invalid_source", message)
                        })?;
                }
                self.add_bytes(
                    source
                        .id
                        .len()
                        .saturating_add(source.title.as_ref().map_or(0, String::len))
                        .saturating_add(source.url.as_ref().map_or(0, String::len)),
                )?;
                self.sources.push(source.clone());
            }
            LanguageEvent::Warning { warning } => {
                if self.warnings.len() >= MAX_WARNINGS {
                    return Err(StreamError::invalid(
                        "stream.too_many_warnings",
                        format!("language stream exceeds {MAX_WARNINGS} warnings"),
                    ));
                }
                validation::identifier("warning.code", &warning.code)
                    .map_err(|message| StreamError::invalid("stream.invalid_warning", message))?;
                validation::safe_text(
                    "warning.message",
                    &warning.message,
                    MAX_WARNING_MESSAGE_BYTES,
                    false,
                )
                .map_err(|message| StreamError::invalid("stream.invalid_warning", message))?;
                self.add_bytes(warning.code.len().saturating_add(warning.message.len()))?;
                self.warnings.push(warning.clone());
            }
            LanguageEvent::Usage { usage } => {
                if self.usage.replace(*usage).is_some() {
                    return Err(StreamError::invalid(
                        "stream.duplicate_usage",
                        "language stream emitted usage more than once",
                    ));
                }
            }
            LanguageEvent::Finished { reason, replay } => {
                if !self.open.is_empty() {
                    return Err(StreamError::invalid(
                        "stream.content_still_open",
                        "language stream finished while a content block remained open",
                    ));
                }
                if let Some(replay) = &replay {
                    replay.validate("replay")?;
                }
                self.finish_reason = Some(reason.clone());
                self.replay.clone_from(replay);
            }
            LanguageEvent::Failed { error, replay } => {
                if let Some(replay) = &replay {
                    replay.validate("replay")?;
                }
                for (index, open) in std::mem::take(&mut self.open) {
                    self.content.insert(index, open.close());
                }
                self.failure = Some(error.clone());
                self.replay.clone_from(replay);
            }
        }
        Ok(())
    }

    /// Returns the complete output after one terminal event.
    pub fn finish(self) -> Result<LanguageOutput, LanguageAssemblyError> {
        if let Some(error) = self.failure {
            return Err(LanguageAssemblyError::Provider {
                error,
                partial: Box::new(LanguagePartialOutput {
                    content: self.content.into_values().collect(),
                    usage: self.usage,
                    replay: self.replay,
                    warnings: self.warnings,
                    sources: self.sources,
                }),
            });
        }
        let finish_reason = self.finish_reason.ok_or_else(|| {
            LanguageAssemblyError::Protocol(StreamError::invalid(
                "stream.missing_finish",
                "language stream ended without a terminal event",
            ))
        })?;
        if self.content.is_empty() {
            return Err(LanguageAssemblyError::Protocol(StreamError::invalid(
                "stream.empty_output",
                "successful language output must contain at least one content block",
            )));
        }
        let has_tool_call = self
            .content
            .values()
            .any(|block| matches!(block, ContentBlock::ToolCall(_)));
        if matches!(finish_reason, FinishReason::ToolCalls) != has_tool_call {
            return Err(LanguageAssemblyError::Protocol(StreamError::invalid(
                "stream.finish_reason_mismatch",
                "tool-call finish reason must match the assembled content",
            )));
        }
        Ok(LanguageOutput {
            content: self.content.into_values().collect(),
            finish_reason,
            usage: self.usage,
            replay: self.replay,
            warnings: self.warnings,
            sources: self.sources,
        })
    }

    fn add_bytes(&mut self, added: usize) -> Result<(), StreamError> {
        let projected = self.assembled_bytes.checked_add(added).ok_or_else(|| {
            StreamError::invalid(
                "stream.output_too_large",
                "language output byte count overflowed",
            )
        })?;
        if projected > MAX_LANGUAGE_OUTPUT_BYTES {
            return Err(StreamError::invalid(
                "stream.output_too_large",
                format!(
                    "language output exceeds the {MAX_LANGUAGE_OUTPUT_BYTES}-byte assembled limit"
                ),
            ));
        }
        self.assembled_bytes = projected;
        Ok(())
    }
}

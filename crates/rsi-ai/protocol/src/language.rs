use std::{fmt, sync::Arc};

mod assembler;
mod stream;
pub use assembler::LanguageAssembler;
pub use stream::LanguageStreamValidator;

use serde::{Deserialize, Serialize, ser::SerializeStruct as _};
use serde_json::Value;
use thiserror::Error;

use crate::{
    AiError, MAX_CONTENT_BLOCKS, MAX_EXTENSION_BYTES, MAX_LANGUAGE_OUTPUT_BYTES, validation,
};

const MAX_SOURCE_FIELD_BYTES: usize = 16 * 1024;
const MAX_WARNING_MESSAGE_BYTES: usize = 4 * 1024;

/// Inclusive token counts reported for one provider attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // Explicit units are part of the durable usage schema.
pub struct TokenUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
}
impl TokenUsage {
    /// Validates inclusive totals and disjoint optional cache/reasoning subsets.
    pub fn new(
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: Option<u64>,
        cache_write_tokens: Option<u64>,
        reasoning_tokens: Option<u64>,
    ) -> Result<Self, StreamError> {
        if input_tokens.checked_add(output_tokens).is_none()
            || cache_read_tokens
                .unwrap_or(0)
                .checked_add(cache_write_tokens.unwrap_or(0))
                .is_none_or(|cached| cached > input_tokens)
            || reasoning_tokens.is_some_and(|reasoning| reasoning > output_tokens)
        {
            return Err(StreamError::invalid(
                "usage.invalid_counters",
                "inclusive totals overflow or a reported subset exceeds its total",
            ));
        }
        Ok(Self {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        })
    }
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }
    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }
    pub const fn cache_read_tokens(&self) -> Option<u64> {
        self.cache_read_tokens
    }
    pub const fn cache_write_tokens(&self) -> Option<u64> {
        self.cache_write_tokens
    }
    pub const fn reasoning_tokens(&self) -> Option<u64> {
        self.reasoning_tokens
    }
}
impl<'de> Deserialize<'de> for TokenUsage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(clippy::struct_field_names)] // Match the exact durable token counters.
        struct Wire {
            input_tokens: u64,
            output_tokens: u64,
            cache_read_tokens: Option<u64>,
            cache_write_tokens: Option<u64>,
            reasoning_tokens: Option<u64>,
        }
        let wire = Wire::deserialize(d)?;
        Self::new(
            wire.input_tokens,
            wire.output_tokens,
            wire.cache_read_tokens,
            wire.cache_write_tokens,
            wire.reasoning_tokens,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Why a language response stopped.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model reached a natural or caller-provided stop condition.
    Stop,
    /// The response ended with one or more tool calls for the caller to execute.
    ToolCalls,
    /// The provider stopped at the configured output-token limit.
    MaxTokens,
    /// Provider content policy stopped the response.
    ContentFilter,
    /// Cancellation ended the response after a normalized terminal event.
    Cancelled,
}

/// One model-requested tool call. Arguments deliberately remain raw text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Provider-neutral identifier used to correlate the later tool result.
    pub id: String,
    /// Exact declared tool name selected by the model.
    pub name: String,
    /// Raw function JSON or freeform input preserved without lossy parsing.
    pub arguments: String,
    /// Provider-neutral syntax used for the call and its later result.
    pub kind: ToolCallKind,
}

/// Provider-neutral syntax used to emit one tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    /// JSON arguments for a function tool.
    Function,
    /// Raw input governed by a freeform grammar.
    Freeform,
}

/// One complete provider-neutral content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    /// User-visible assistant text.
    Text { text: String },
    /// Provider-exposed reasoning text kept distinct from visible output.
    Reasoning { text: String },
    /// One complete model-requested tool call.
    ToolCall(ToolCall),
}

/// Metadata that opens one streamed content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentStart {
    /// Opens a user-visible text block.
    Text,
    /// Opens a provider-exposed reasoning block.
    Reasoning,
    /// Opens a tool call whose arguments arrive as deltas.
    ToolCall {
        /// Correlation identifier for the later tool result.
        id: String,
        /// Exact declared tool name.
        name: String,
        /// Exact provider-neutral call syntax.
        kind: ToolCallKind,
    },
}

/// One incremental update for an open content block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ContentDelta {
    /// Appends visible text to an open text block.
    Text(String),
    /// Appends reasoning text to an open reasoning block.
    Reasoning(String),
    /// Appends raw JSON text to an open tool-call block.
    ToolArguments(String),
}

/// One citeable source returned by a provider-hosted operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Provider-neutral source identifier unique within the response.
    pub id: String,
    pub title: Option<String>,
    pub url: Option<String>,
}

/// A safe provider or compatibility warning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warning {
    /// Stable bounded warning code.
    pub code: String,
    /// Safe bounded explanation for callers.
    pub message: String,
}

/// Bounded provider-private JSON that never contains binary data or secrets.
#[derive(Clone, PartialEq)]
pub struct ProviderExtension {
    inner: Arc<ProviderExtensionInner>,
}

#[derive(PartialEq)]
struct ProviderExtensionInner {
    namespace: String,
    version: u32,
    value: Value,
    encoded_len: usize,
}

impl fmt::Debug for ProviderExtension {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderExtension")
            .field("namespace", &self.inner.namespace)
            .field("version", &self.inner.version)
            .field("value", &self.inner.value)
            .finish()
    }
}

impl Serialize for ProviderExtension {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut wire = serializer.serialize_struct("ProviderExtension", 3)?;
        wire.serialize_field("namespace", &self.inner.namespace)?;
        wire.serialize_field("version", &self.inner.version)?;
        wire.serialize_field("value", &self.inner.value)?;
        wire.end()
    }
}

impl<'de> Deserialize<'de> for ProviderExtension {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireExtension {
            namespace: String,
            version: u32,
            value: Value,
        }

        let wire = WireExtension::deserialize(deserializer)?;
        Self::new(wire.namespace, wire.version, wire.value).map_err(serde::de::Error::custom)
    }
}

impl ProviderExtension {
    /// Creates one validated immutable provider extension.
    pub fn new(
        namespace: impl Into<String>,
        version: u32,
        value: Value,
    ) -> Result<Self, StreamError> {
        #[derive(Serialize)]
        struct WireExtension<'a> {
            namespace: &'a str,
            version: u32,
            value: &'a Value,
        }

        let namespace = namespace.into();
        validation::identifier("provider_extension.namespace", &namespace)
            .map_err(|message| StreamError::invalid("stream.invalid_extension", message))?;
        validation::validate_json_structure(&value)
            .map_err(|error| StreamError::invalid("stream.invalid_extension", error.to_string()))?;
        let encoded_len = validation::encoded_len(&WireExtension {
            namespace: &namespace,
            version,
            value: &value,
        })
        .map_err(|error| StreamError::invalid("stream.invalid_extension", error))?;
        if encoded_len > MAX_EXTENSION_BYTES {
            return Err(StreamError::invalid(
                "stream.extension_too_large",
                format!("provider extension exceeds the {MAX_EXTENSION_BYTES}-byte encoded limit"),
            ));
        }
        Ok(Self {
            inner: Arc::new(ProviderExtensionInner {
                namespace,
                version,
                value,
                encoded_len,
            }),
        })
    }

    /// Returns the provider-family namespace.
    pub fn namespace(&self) -> &str {
        &self.inner.namespace
    }

    /// Returns the namespace-local format version.
    pub fn version(&self) -> u32 {
        self.inner.version
    }

    /// Returns the bounded provider-private JSON value.
    pub fn value(&self) -> &Value {
        &self.inner.value
    }

    /// Returns the cached exact JSON wire length.
    pub fn encoded_len(&self) -> usize {
        self.inner.encoded_len
    }
}

/// Normalized language events emitted by every adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LanguageEvent {
    /// Opens the next indexed content block.
    ContentStarted {
        /// Zero-based contiguous block index.
        index: u32,
        /// Metadata fixing the block's content kind.
        content: ContentStart,
    },
    /// Appends data to an open content block.
    ContentDelta {
        /// Index of the open block receiving the delta.
        index: u32,
        /// Kind-matched incremental content.
        delta: ContentDelta,
    },
    /// Closes an open content block.
    ContentFinished {
        /// Index of the block to close.
        index: u32,
    },
    /// Adds one citeable source to the response.
    Source { source: Source },
    /// Adds one non-terminal compatibility or provider warning.
    Warning { warning: Warning },
    /// Supplies the response's sole cumulative usage record.
    Usage { usage: TokenUsage },
    /// Terminates a successful response after every content block is closed.
    Finished {
        reason: FinishReason,
        /// Optional bounded state needed to replay provider reasoning or tool context.
        replay: Option<ProviderExtension>,
    },
    /// Terminates a failed response and exposes validated partial output for
    /// diagnostics, never as a successful [`LanguageOutput`].
    Failed {
        error: AiError,
        /// Optional bounded provider state observed before failure.
        replay: Option<ProviderExtension>,
    },
}

impl LanguageEvent {
    /// Validates the context-free fields and individual bounds of one event.
    /// Stream ordering and aggregate limits remain owned by
    /// [`LanguageStreamValidator`].
    ///
    /// # Errors
    ///
    /// Returns a stable stream error when one field could not belong to any
    /// valid normalized language stream.
    pub fn validate(&self) -> Result<(), StreamError> {
        let validate_index = |index: u32| {
            if usize::try_from(index).map_or(true, |index| index >= MAX_CONTENT_BLOCKS) {
                Err(StreamError::invalid(
                    "stream.too_many_blocks",
                    format!("content index exceeds the {MAX_CONTENT_BLOCKS}-block limit"),
                ))
            } else {
                Ok(())
            }
        };
        match self {
            Self::ContentStarted { index, content } => {
                validate_index(*index)?;
                if let ContentStart::ToolCall { id, name, .. } = content {
                    validation::identifier("tool_call.id", id).map_err(|message| {
                        StreamError::invalid("stream.invalid_tool_call", message)
                    })?;
                    validation::tool_name("tool_call.name", name).map_err(|message| {
                        StreamError::invalid("stream.invalid_tool_call", message)
                    })?;
                }
                Ok(())
            }
            Self::ContentDelta { index, delta } => {
                validate_index(*index)?;
                let bytes = match delta {
                    ContentDelta::Text(value)
                    | ContentDelta::Reasoning(value)
                    | ContentDelta::ToolArguments(value) => value.len(),
                };
                if bytes == 0 {
                    return Err(StreamError::invalid(
                        "stream.empty_delta",
                        "language content deltas must make nonempty progress",
                    ));
                }
                if bytes > MAX_LANGUAGE_OUTPUT_BYTES {
                    return Err(StreamError::invalid(
                        "stream.output_too_large",
                        format!(
                            "one language delta exceeds the {MAX_LANGUAGE_OUTPUT_BYTES}-byte per-event limit"
                        ),
                    ));
                }
                Ok(())
            }
            Self::ContentFinished { index } => validate_index(*index),
            Self::Source { source } => {
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
                Ok(())
            }
            Self::Warning { warning } => {
                validation::identifier("warning.code", &warning.code)
                    .map_err(|message| StreamError::invalid("stream.invalid_warning", message))?;
                validation::safe_text(
                    "warning.message",
                    &warning.message,
                    MAX_WARNING_MESSAGE_BYTES,
                    false,
                )
                .map_err(|message| StreamError::invalid("stream.invalid_warning", message))
            }
            Self::Usage { .. } | Self::Finished { .. } => Ok(()),
            Self::Failed { error, .. } => error.validate().map_err(|error| {
                StreamError::invalid("stream.invalid_provider_error", error.to_string())
            }),
        }
    }
}

/// Complete language output assembled through the same grammar callers stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageOutput {
    /// Ordered complete content blocks.
    pub content: Vec<ContentBlock>,
    /// Successful terminal reason.
    pub finish_reason: FinishReason,
    /// Sole cumulative usage record, when reported.
    pub usage: Option<TokenUsage>,
    /// Optional provider state required for a later request replay.
    pub replay: Option<ProviderExtension>,
    /// Ordered safe warnings retained from the attempt.
    pub warnings: Vec<Warning>,
    /// Ordered citeable sources retained from the attempt.
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
                        kind: call.kind,
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
            LanguageAssemblyError::Incomplete { .. } => StreamError::invalid(
                "stream.incomplete",
                "incomplete output is not a successful LanguageOutput",
            ),
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
    /// Complete content blocks accepted before failure.
    pub content: Vec<ContentBlock>,
    /// Usage observed before failure, when reported.
    pub usage: Option<TokenUsage>,
    /// Optional provider state observed before failure.
    pub replay: Option<ProviderExtension>,
    /// Safe warnings observed before failure.
    pub warnings: Vec<Warning>,
    /// Sources observed before failure.
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

/// Terminal language assembly failure. Provider partial output is diagnostic
/// evidence and never a successful [`LanguageOutput`].
#[derive(Clone, Debug, Error, PartialEq)]
pub enum LanguageAssemblyError {
    /// The normalized event stream violated ordering, bounds, or terminal grammar.
    #[error(transparent)]
    Protocol(#[from] StreamError),
    /// The provider reached its output budget; this content cannot authorize Tools.
    #[error("language output reached its token limit")]
    Incomplete {
        /// Valid bounded diagnostic content and usage.
        partial: Box<LanguagePartialOutput>,
    },
    /// The provider emitted a semantic failure after some valid output.
    #[error("{error}")]
    Provider {
        error: AiError,
        /// Valid normalized output accepted before the failure, for diagnostics.
        partial: Box<LanguagePartialOutput>,
    },
}

impl LanguageAssemblyError {
    /// Stable code for protocol failures, or a provider-neutral error kind.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Protocol(error) => error.code(),
            Self::Incomplete { .. } => "stream.incomplete",
            Self::Provider { error, .. } => error.kind().code(),
        }
    }
}

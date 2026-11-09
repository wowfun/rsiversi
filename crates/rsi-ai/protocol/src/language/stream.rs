use super::{ContentDelta, ContentStart, FinishReason, LanguageEvent, StreamError};
use crate::{MAX_LANGUAGE_OUTPUT_BYTES, MAX_SOURCES, MAX_WARNINGS};

#[derive(Clone, Copy, Debug)]
enum BlockKind {
    Text,
    Reasoning,
    ToolCall,
}

/// Incremental stream grammar without retaining event payloads.
///
/// A valid stream may describe failure or incomplete output; validation is not
/// execution authority. Discard this validator after any error.
#[derive(Debug, Default)]
pub struct LanguageStreamValidator {
    blocks: Vec<Option<BlockKind>>,
    open_blocks: usize,
    assembled_bytes: usize,
    sources: usize,
    warnings: usize,
    usage_seen: bool,
    has_tool_call: bool,
    pub(super) finish_reason: Option<FinishReason>,
    failed: bool,
}

impl LanguageStreamValidator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            blocks: Vec::new(),
            open_blocks: 0,
            assembled_bytes: 0,
            sources: 0,
            warnings: 0,
            usage_seen: false,
            has_tool_call: false,
            finish_reason: None,
            failed: false,
        }
    }

    /// Checks one event's fields, ordering, block type, and aggregate bounds.
    #[allow(clippy::too_many_lines)] // One exhaustive transition owns the stream grammar.
    #[allow(clippy::missing_panics_doc)] // Event field validation establishes index conversion bounds.
    pub fn push(&mut self, event: &LanguageEvent) -> Result<(), StreamError> {
        if self.finish_reason.is_some() || self.failed {
            return Err(StreamError::invalid(
                "stream.already_finished",
                "language stream emitted an event after its terminal event",
            ));
        }
        event.validate()?;
        match event {
            LanguageEvent::ContentStarted { index, content } => {
                if usize::try_from(*index).expect("validated index fits usize") != self.blocks.len()
                {
                    return Err(StreamError::invalid(
                        "stream.non_contiguous_index",
                        format!(
                            "content index {index} is not the next contiguous index {}",
                            self.blocks.len()
                        ),
                    ));
                }
                let kind = match content {
                    ContentStart::Text => BlockKind::Text,
                    ContentStart::Reasoning => BlockKind::Reasoning,
                    ContentStart::ToolCall { id, name, .. } => {
                        self.add_bytes(id.len().saturating_add(name.len()))?;
                        self.has_tool_call = true;
                        BlockKind::ToolCall
                    }
                };
                self.blocks.push(Some(kind));
                self.open_blocks += 1;
            }
            LanguageEvent::ContentDelta { index, delta } => {
                let added = match (*self.open_block(*index)?, delta) {
                    (BlockKind::Text, ContentDelta::Text(value))
                    | (BlockKind::Reasoning, ContentDelta::Reasoning(value))
                    | (BlockKind::ToolCall, ContentDelta::ToolArguments(value)) => value.len(),
                    _ => {
                        return Err(StreamError::invalid(
                            "stream.content_type_mismatch",
                            "content delta does not match the open block type",
                        ));
                    }
                };
                self.add_bytes(added)?;
            }
            LanguageEvent::ContentFinished { index } => {
                self.open_block(*index)?;
                self.blocks[usize::try_from(*index).expect("validated index fits usize")] = None;
                self.open_blocks -= 1;
            }
            LanguageEvent::Source { source } => {
                if self.sources >= MAX_SOURCES {
                    return Err(StreamError::invalid(
                        "stream.too_many_sources",
                        format!("language stream exceeds {MAX_SOURCES} sources"),
                    ));
                }
                self.add_bytes(
                    source
                        .id
                        .len()
                        .saturating_add(source.title.as_ref().map_or(0, String::len))
                        .saturating_add(source.url.as_ref().map_or(0, String::len)),
                )?;
                self.sources += 1;
            }
            LanguageEvent::Warning { warning } => {
                if self.warnings >= MAX_WARNINGS {
                    return Err(StreamError::invalid(
                        "stream.too_many_warnings",
                        format!("language stream exceeds {MAX_WARNINGS} warnings"),
                    ));
                }
                self.add_bytes(warning.code.len().saturating_add(warning.message.len()))?;
                self.warnings += 1;
            }
            LanguageEvent::Usage { .. } => {
                if self.usage_seen {
                    return Err(StreamError::invalid(
                        "stream.duplicate_usage",
                        "language stream emitted usage more than once",
                    ));
                }
                self.usage_seen = true;
            }
            LanguageEvent::Finished { reason, .. } => {
                if self.open_blocks != 0
                    && !matches!(reason, FinishReason::MaxTokens | FinishReason::Cancelled)
                {
                    return Err(StreamError::invalid(
                        "stream.content_still_open",
                        "language stream finished while a content block remained open",
                    ));
                }
                self.finish_reason = Some(reason.clone());
            }
            LanguageEvent::Failed { .. } => self.failed = true,
        }
        Ok(())
    }

    /// Checks complete-stream grammar, including failed and incomplete evidence.
    pub fn finish(&self) -> Result<(), StreamError> {
        if self.failed {
            return Ok(());
        }
        let reason = self.finish_reason.as_ref().ok_or_else(|| {
            StreamError::invalid(
                "stream.missing_finish",
                "language stream ended without a terminal event",
            )
        })?;
        if matches!(reason, FinishReason::MaxTokens | FinishReason::Cancelled) {
            return Ok(());
        }
        if self.blocks.is_empty() {
            return Err(StreamError::invalid(
                "stream.empty_output",
                "successful language output must contain at least one content block",
            ));
        }
        if matches!(reason, FinishReason::ToolCalls) != self.has_tool_call {
            return Err(StreamError::invalid(
                "stream.finish_reason_mismatch",
                "tool-call finish reason must match the assembled content",
            ));
        }
        Ok(())
    }

    fn open_block(&self, index: u32) -> Result<&BlockKind, StreamError> {
        self.blocks
            .get(usize::try_from(index).expect("validated index fits usize"))
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                StreamError::invalid(
                    "stream.content_not_started",
                    format!("content index {index} is not open"),
                )
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

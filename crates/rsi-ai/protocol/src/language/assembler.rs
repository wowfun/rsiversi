use serde::Serialize;

use super::{
    AiError, ContentBlock, ContentDelta, ContentStart, FinishReason, LanguageAssemblyError,
    LanguageEvent, LanguageOutput, LanguagePartialOutput, LanguageStreamValidator,
    ProviderExtension, Source, StreamError, TokenUsage, ToolCall, Warning, validation,
};

/// Collects output after applying the shared stream grammar.
/// Serialization exposes diagnostic state, not a resumable checkpoint. Restore
/// reconstructs an assembler only by applying validated events.
#[derive(Debug, Default, Serialize)]
pub struct LanguageAssembler {
    #[serde(skip)]
    grammar: LanguageStreamValidator,
    content: Vec<ContentBlock>,
    #[serde(skip)]
    encoded_payload_bytes: usize,
    usage: Option<TokenUsage>,
    replay: Option<ProviderExtension>,
    failure: Option<AiError>,
    warnings: Vec<Warning>,
    sources: Vec<Source>,
}

impl LanguageAssembler {
    /// Conservative encoded weight maintained at ingestion without rescanning
    /// accumulated strings. Fixed overhead bounds field names and counters.
    pub fn retained_encoded_weight(&self) -> usize {
        1024 + self.encoded_payload_bytes
            + self.content.len() * 256
            + self.sources.len()
            + self.warnings.len()
    }

    #[must_use]
    pub const fn new() -> Self {
        Self {
            grammar: LanguageStreamValidator::new(),
            content: Vec::new(),
            encoded_payload_bytes: 0,
            usage: None,
            replay: None,
            failure: None,
            warnings: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// Checks grammar and bounds before retaining one normalized event.
    #[allow(clippy::missing_panics_doc)] // Successful grammar validation establishes every index.
    pub fn push(&mut self, event: &LanguageEvent) -> Result<(), StreamError> {
        self.grammar.push(event)?;
        match event {
            LanguageEvent::ContentStarted { content, .. } => {
                let block = match content {
                    ContentStart::Text => ContentBlock::Text {
                        text: String::new(),
                    },
                    ContentStart::Reasoning => ContentBlock::Reasoning {
                        text: String::new(),
                    },
                    ContentStart::ToolCall { id, name, kind } => {
                        self.count_encoded(&(id, name))?;
                        ContentBlock::ToolCall(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: String::new(),
                            kind: *kind,
                        })
                    }
                };
                self.content.push(block);
            }
            LanguageEvent::ContentDelta { index, delta } => {
                let (ContentDelta::Text(value)
                | ContentDelta::Reasoning(value)
                | ContentDelta::ToolArguments(value)) = delta;
                self.count_encoded(value)?;
                // The grammar established this index and its matching content kind.
                match &mut self.content
                    [usize::try_from(*index).expect("validated index fits usize")]
                {
                    ContentBlock::Text { text } | ContentBlock::Reasoning { text } => {
                        text.push_str(value);
                    }
                    ContentBlock::ToolCall(call) => call.arguments.push_str(value),
                }
            }
            LanguageEvent::ContentFinished { .. } => {}
            LanguageEvent::Source { source } => {
                self.count_encoded(source)?;
                self.sources.push(source.clone());
            }
            LanguageEvent::Warning { warning } => {
                self.count_encoded(warning)?;
                self.warnings.push(warning.clone());
            }
            LanguageEvent::Usage { usage } => self.usage = Some(*usage),
            LanguageEvent::Finished { replay, .. } => {
                self.count_encoded(replay)?;
                self.replay.clone_from(replay);
            }
            LanguageEvent::Failed { error, replay } => {
                self.count_encoded(&(replay, error))?;
                self.failure = Some(error.clone());
                self.replay.clone_from(replay);
            }
        }
        Ok(())
    }

    /// Returns complete output or bounded partial evidence after a terminal event.
    #[allow(clippy::missing_panics_doc)] // The validated non-failed terminal carries its reason.
    pub fn finish(self) -> Result<LanguageOutput, LanguageAssemblyError> {
        self.grammar.finish()?;
        if let Some(error) = self.failure {
            return Err(LanguageAssemblyError::Provider {
                error,
                partial: Box::new(LanguagePartialOutput {
                    content: self.content,
                    usage: self.usage,
                    replay: self.replay,
                    warnings: self.warnings,
                    sources: self.sources,
                }),
            });
        }
        let finish_reason = self
            .grammar
            .finish_reason
            .expect("validated non-failed terminal has a finish reason");
        if finish_reason == FinishReason::MaxTokens {
            return Err(LanguageAssemblyError::Incomplete {
                partial: Box::new(LanguagePartialOutput {
                    content: self.content,
                    usage: self.usage,
                    replay: self.replay,
                    warnings: self.warnings,
                    sources: self.sources,
                }),
            });
        }
        Ok(LanguageOutput {
            content: self.content,
            finish_reason,
            usage: self.usage,
            replay: self.replay,
            warnings: self.warnings,
            sources: self.sources,
        })
    }

    fn count_encoded<T: Serialize>(&mut self, value: &T) -> Result<(), StreamError> {
        let bytes = validation::encoded_len(value)
            .map_err(|error| StreamError::invalid("stream.encoding", error))?;
        self.encoded_payload_bytes = self
            .encoded_payload_bytes
            .checked_add(bytes)
            .ok_or_else(|| StreamError::invalid("stream.encoding", "encoded weight overflow"))?;
        Ok(())
    }
}

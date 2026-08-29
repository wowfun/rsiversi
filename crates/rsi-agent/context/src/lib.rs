//! Incremental Fact-to-Language projection with complete-turn compaction.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use rsi_agent_session_protocol::{EffectId, SessionFact, SessionFactBody, SessionHeader, TurnId};
use rsi_ai_protocol::{
    ContentBlock, LanguageAssembler, LanguageAssemblyError, LanguageRequest, Message,
    MessageContent, ToolChoice,
};
use rsi_media_protocol::{MediaDescriptor, MediaKind};
use rsi_tools_protocol::{ToolContent, ToolDefinition, ToolResult};
use std::collections::BTreeMap;
use thiserror::Error;

/// Default maximum projected Language messages.
pub const DEFAULT_CONTEXT_MESSAGES: usize = 256;
/// Default maximum canonical encoded message bytes.
pub const DEFAULT_CONTEXT_BYTES: usize = 8 * 1024 * 1024;
/// Absolute projected message bound.
pub const MAXIMUM_CONTEXT_MESSAGES: usize = 4_096;
/// Absolute projected byte bound.
pub const MAXIMUM_CONTEXT_BYTES: usize = 32 * 1024 * 1024;

/// Explicit compaction limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextLimits {
    /// Maximum projected messages including system and omission notice.
    pub max_messages: usize,
    /// Maximum canonical encoded message bytes.
    pub max_bytes: usize,
}

impl ContextLimits {
    /// Creates bounded nonzero limits.
    pub fn new(max_messages: usize, max_bytes: usize) -> Result<Self> {
        if max_messages == 0
            || max_messages > MAXIMUM_CONTEXT_MESSAGES
            || max_bytes == 0
            || max_bytes > MAXIMUM_CONTEXT_BYTES
        {
            return Err(ContextError::Invalid(
                "context limits are zero or exceed the absolute bounds".into(),
            ));
        }
        Ok(Self {
            max_messages,
            max_bytes,
        })
    }
}

impl Default for ContextLimits {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_CONTEXT_MESSAGES,
            max_bytes: DEFAULT_CONTEXT_BYTES,
        }
    }
}

/// Complete bounded projection for one model call.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelContext {
    /// Provider-neutral ordered messages.
    pub messages: Vec<Message>,
    /// Number of complete oldest turns omitted as one unit.
    pub omitted_turns: usize,
    /// Highest applied Fact sequence.
    pub through_seq: u64,
}

/// Incremental fold over one immutable session header and its Facts.
#[derive(Debug)]
pub struct ContextFold {
    header: SessionHeader,
    through_seq: u64,
    omitted_turns: usize,
    retention_limits: Option<ContextLimits>,
    turns: Vec<ProjectedTurn>,
    turn_index: BTreeMap<TurnId, usize>,
    assemblers: BTreeMap<EffectId, ActiveAssembler>,
    retained_messages: usize,
    retained_message_bytes: usize,
}

#[derive(Debug)]
struct ProjectedTurn {
    messages: Vec<Message>,
    message_bytes: usize,
    terminal: bool,
}

#[derive(Debug)]
struct ActiveAssembler {
    turn_id: TurnId,
    assembler: LanguageAssembler,
}

impl ContextFold {
    /// Starts an empty projection for one immutable session.
    pub fn new(header: SessionHeader) -> Result<Self> {
        header
            .validate()
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        Ok(Self {
            header,
            through_seq: 0,
            omitted_turns: 0,
            retention_limits: None,
            turns: Vec::new(),
            turn_index: BTreeMap::new(),
            assemblers: BTreeMap::new(),
            retained_messages: 0,
            retained_message_bytes: 0,
        })
    }

    /// Starts an incremental projection that discards complete old turns as it folds.
    pub fn with_limits(header: SessionHeader, limits: ContextLimits) -> Result<Self> {
        ContextLimits::new(limits.max_messages, limits.max_bytes)?;
        let mut fold = Self::new(header)?;
        fold.retention_limits = Some(limits);
        Ok(fold)
    }

    /// Returns the immutable source header.
    pub const fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Returns the highest contiguous Fact already applied.
    pub const fn through_seq(&self) -> u64 {
        self.through_seq
    }

    /// Applies an exact contiguous suffix once.
    pub fn apply(&mut self, facts: &[SessionFact]) -> Result<()> {
        let mut expected = self
            .through_seq
            .checked_add(1)
            .ok_or_else(|| ContextError::Invalid("Fact sequence exhausted".into()))?;
        for fact in facts {
            if fact.seq() != expected {
                return Err(ContextError::Invalid(format!(
                    "context expected Fact {expected}, got {}",
                    fact.seq()
                )));
            }
            fact.validate()
                .map_err(|error| ContextError::Invalid(error.to_string()))?;
            self.apply_body(fact.body())?;
            self.through_seq = fact.seq();
            self.compact_retained()?;
            expected = expected
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("Fact sequence exhausted".into()))?;
        }
        Ok(())
    }

    /// Applies visible Facts while advancing across claim-hidden sequence holes.
    pub fn apply_page(&mut self, facts: &[SessionFact], through_seq: u64) -> Result<()> {
        if through_seq < self.through_seq {
            return Err(ContextError::Invalid(
                "claim page watermark moved backwards".into(),
            ));
        }
        let mut previous = self.through_seq;
        for fact in facts {
            if fact.seq() <= previous || fact.seq() > through_seq {
                return Err(ContextError::Invalid(
                    "claim page Facts are not increasing within its watermark".into(),
                ));
            }
            fact.validate()
                .map_err(|error| ContextError::Invalid(error.to_string()))?;
            self.apply_body(fact.body())?;
            previous = fact.seq();
            self.compact_retained()?;
        }
        self.through_seq = through_seq;
        Ok(())
    }

    /// Projects bounded messages, dropping only complete oldest turns.
    pub fn project(&self, limits: ContextLimits) -> Result<ModelContext> {
        ContextLimits::new(limits.max_messages, limits.max_bytes)?;
        let system = if self.header.profile().system_prompt().is_empty() {
            None
        } else {
            Some(
                Message::system_text(self.header.profile().system_prompt())
                    .map_err(|error| ContextError::Invalid(error.to_string()))?,
            )
        };
        let system_bytes = system
            .as_ref()
            .map(encoded_message_bytes)
            .transpose()?
            .unwrap_or(0);
        let mut retained_messages = usize::from(system.is_some())
            .checked_add(self.retained_messages)
            .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
        let mut retained_message_bytes = system_bytes
            .checked_add(self.retained_message_bytes)
            .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        let turn_sizes = self
            .turns
            .iter()
            .map(|turn| (turn.messages.len(), turn.message_bytes))
            .collect::<Vec<_>>();
        let mut omitted = self.omitted_turns;
        let mut skipped_retained = 0_usize;
        loop {
            let notice = (omitted > 0)
                .then(|| {
                    Message::developer_text(format!(
                        "[Context omitted {omitted} complete earlier turn(s).]"
                    ))
                    .map_err(|error| ContextError::Invalid(error.to_string()))
                })
                .transpose()?;
            let notice_bytes = notice
                .as_ref()
                .map(encoded_message_bytes)
                .transpose()?
                .unwrap_or(0);
            let message_count = retained_messages
                .checked_add(usize::from(notice.is_some()))
                .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
            let message_bytes = retained_message_bytes
                .checked_add(notice_bytes)
                .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
            if message_count <= limits.max_messages
                && encoded_array_bytes(message_count, message_bytes)? <= limits.max_bytes
            {
                let mut messages = Vec::with_capacity(message_count);
                messages.extend(system.iter().cloned());
                messages.extend(notice);
                messages.extend(
                    self.turns
                        .iter()
                        .skip(skipped_retained)
                        .flat_map(|turn| turn.messages.iter().cloned()),
                );
                return Ok(ModelContext {
                    messages,
                    omitted_turns: omitted,
                    through_seq: self.through_seq,
                });
            }
            let Some(turn) = self.turns.get(skipped_retained) else {
                return Err(ContextError::TooLarge);
            };
            if !turn.terminal {
                return Err(ContextError::TooLarge);
            }
            let (removed_messages, removed_bytes) = turn_sizes[skipped_retained];
            retained_messages = retained_messages
                .checked_sub(removed_messages)
                .ok_or_else(|| ContextError::Invalid("context message count underflowed".into()))?;
            retained_message_bytes = retained_message_bytes
                .checked_sub(removed_bytes)
                .ok_or_else(|| ContextError::Invalid("context byte count underflowed".into()))?;
            omitted = omitted
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("omitted turn count overflowed".into()))?;
            skipped_retained = skipped_retained
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("retained turn index overflowed".into()))?;
        }
    }

    fn compact_retained(&mut self) -> Result<()> {
        let Some(limits) = self.retention_limits else {
            return Ok(());
        };
        while self.retained_shape_exceeds(limits)? {
            if self.turns.first().is_none_or(|turn| !turn.terminal) {
                break;
            }
            let removed = self.turns.remove(0);
            self.retained_messages = self
                .retained_messages
                .checked_sub(removed.messages.len())
                .ok_or_else(|| ContextError::Invalid("context message count underflowed".into()))?;
            self.retained_message_bytes = self
                .retained_message_bytes
                .checked_sub(removed.message_bytes)
                .ok_or_else(|| ContextError::Invalid("context byte count underflowed".into()))?;
            self.omitted_turns = self
                .omitted_turns
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("omitted turn count overflowed".into()))?;
            for turn_id in self.turn_index.keys().cloned().collect::<Vec<_>>() {
                let index = self
                    .turn_index
                    .get_mut(&turn_id)
                    .expect("collected turn index exists");
                if *index == 0 {
                    self.turn_index.remove(&turn_id);
                } else {
                    *index -= 1;
                }
            }
            drop(removed);
        }
        Ok(())
    }

    fn retained_shape_exceeds(&self, limits: ContextLimits) -> Result<bool> {
        let system_messages = usize::from(!self.header.profile().system_prompt().is_empty());
        let count = system_messages
            .checked_add(usize::from(self.omitted_turns > 0))
            .and_then(|count| count.checked_add(self.retained_messages))
            .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
        let mut bytes = if self.header.profile().system_prompt().is_empty() {
            0
        } else {
            encoded_message_bytes(
                &Message::system_text(self.header.profile().system_prompt())
                    .map_err(|error| ContextError::Invalid(error.to_string()))?,
            )?
        };
        if self.omitted_turns > 0 {
            bytes = bytes
                .checked_add(encoded_message_bytes(
                    &Message::developer_text(format!(
                        "[Context omitted {} complete earlier turn(s).]",
                        self.omitted_turns
                    ))
                    .map_err(|error| ContextError::Invalid(error.to_string()))?,
                )?)
                .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        }
        bytes = bytes
            .checked_add(self.retained_message_bytes)
            .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        Ok(count > limits.max_messages || encoded_array_bytes(count, bytes)? > limits.max_bytes)
    }

    /// Builds one Language request with the exact active Tool definitions.
    pub fn request(
        &self,
        limits: ContextLimits,
        tools: Vec<ToolDefinition>,
    ) -> Result<LanguageRequest> {
        let projected = self.project(limits)?;
        let request = LanguageRequest::new(projected.messages)
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        if tools.is_empty() {
            Ok(request)
        } else {
            request
                .with_tools(tools, ToolChoice::Auto)
                .map_err(|error| ContextError::Invalid(error.to_string()))
        }
    }

    fn apply_body(&mut self, body: &SessionFactBody) -> Result<()> {
        match body {
            SessionFactBody::TurnAccepted { turn_id, text, .. } => {
                let message = Message::user_text(text)
                    .map_err(|error| ContextError::Invalid(error.to_string()))?;
                self.insert_turn(turn_id, message)?;
            }
            SessionFactBody::ImageRequested {
                turn_id, request, ..
            } => {
                let message = Message::user_text(request.prompt())
                    .map_err(|error| ContextError::Invalid(error.to_string()))?;
                self.insert_turn(turn_id, message)?;
            }
            SessionFactBody::ModelIntent {
                turn_id, effect_id, ..
            } => {
                self.require_live_turn(turn_id)?;
                if self
                    .assemblers
                    .insert(
                        effect_id.clone(),
                        ActiveAssembler {
                            turn_id: turn_id.clone(),
                            assembler: LanguageAssembler::new(),
                        },
                    )
                    .is_some()
                {
                    return Err(ContextError::Invalid(
                        "model effect intent was duplicated".into(),
                    ));
                }
            }
            SessionFactBody::ModelEvent {
                turn_id,
                effect_id,
                event,
            } => self.apply_model_event(turn_id, effect_id, event)?,
            SessionFactBody::ToolResult {
                turn_id,
                identity,
                result,
                ..
            } => {
                let message = tool_message(identity.call_id(), result)?;
                self.push_turn_message(turn_id, message)?;
            }
            SessionFactBody::ImageOutput { turn_id, media, .. } => {
                let descriptor = media_descriptor(media)?;
                let message = Message::assistant(vec![MessageContent::Image(descriptor)])
                    .map_err(|error| ContextError::Invalid(error.to_string()))?;
                self.push_turn_message(turn_id, message)?;
            }
            SessionFactBody::TurnTerminal { turn_id, .. } => {
                let turn = self.turn_mut(turn_id)?;
                if turn.terminal {
                    return Err(ContextError::Invalid(
                        "turn received more than one terminal Fact".into(),
                    ));
                }
                turn.terminal = true;
            }
            SessionFactBody::CancelRequested { .. }
            | SessionFactBody::ModelStarted { .. }
            | SessionFactBody::ImageIntent { .. }
            | SessionFactBody::ImageStarted { .. }
            | SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolStarted { .. } => {}
        }
        Ok(())
    }

    fn apply_model_event(
        &mut self,
        turn_id: &TurnId,
        effect_id: &EffectId,
        event: &rsi_ai_protocol::LanguageEvent,
    ) -> Result<()> {
        let terminal = matches!(
            event,
            rsi_ai_protocol::LanguageEvent::Finished { .. }
                | rsi_ai_protocol::LanguageEvent::Failed { .. }
        );
        let active = self
            .assemblers
            .get_mut(effect_id)
            .ok_or_else(|| ContextError::Invalid("model event has no matching intent".into()))?;
        if &active.turn_id != turn_id {
            return Err(ContextError::Invalid(
                "model event changed its owning turn".into(),
            ));
        }
        active
            .assembler
            .push(event)
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        if !terminal {
            return Ok(());
        }

        let active = self
            .assemblers
            .remove(effect_id)
            .expect("assembler was observed above");
        match active.assembler.finish() {
            Ok(output) => {
                let message = assistant_message(output.content, output.replay.as_ref())?;
                self.push_turn_message(turn_id, message)?;
                Ok(())
            }
            Err(LanguageAssemblyError::Provider { .. }) => Ok(()),
            Err(LanguageAssemblyError::Protocol(error)) => {
                Err(ContextError::Invalid(error.to_string()))
            }
        }
    }

    fn require_live_turn(&self, turn_id: &TurnId) -> Result<()> {
        let index = self
            .turn_index
            .get(turn_id)
            .copied()
            .ok_or_else(|| ContextError::Invalid("Fact references an unknown turn".into()))?;
        if self.turns[index].terminal {
            return Err(ContextError::Invalid(
                "Fact references a terminal turn".into(),
            ));
        }
        Ok(())
    }

    fn insert_turn(&mut self, turn_id: &TurnId, message: Message) -> Result<()> {
        if self.turn_index.contains_key(turn_id) {
            return Err(ContextError::Invalid(
                "turn was accepted more than once".into(),
            ));
        }
        let message_bytes = encoded_message_bytes(&message)?;
        let retained_messages = self
            .retained_messages
            .checked_add(1)
            .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
        let retained_message_bytes = self
            .retained_message_bytes
            .checked_add(message_bytes)
            .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        let index = self.turns.len();
        self.turns.push(ProjectedTurn {
            messages: vec![message],
            message_bytes,
            terminal: false,
        });
        self.turn_index.insert(turn_id.clone(), index);
        self.retained_messages = retained_messages;
        self.retained_message_bytes = retained_message_bytes;
        Ok(())
    }

    fn push_turn_message(&mut self, turn_id: &TurnId, message: Message) -> Result<()> {
        let index = self
            .turn_index
            .get(turn_id)
            .copied()
            .ok_or_else(|| ContextError::Invalid("Fact references an unknown turn".into()))?;
        let message_bytes = encoded_message_bytes(&message)?;
        let retained_messages = self
            .retained_messages
            .checked_add(1)
            .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
        let retained_message_bytes = self
            .retained_message_bytes
            .checked_add(message_bytes)
            .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        let turn = self
            .turns
            .get_mut(index)
            .ok_or_else(|| ContextError::Invalid("turn index is corrupt".into()))?;
        turn.message_bytes = turn
            .message_bytes
            .checked_add(message_bytes)
            .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        turn.messages.push(message);
        self.retained_messages = retained_messages;
        self.retained_message_bytes = retained_message_bytes;
        Ok(())
    }

    fn turn_mut(&mut self, turn_id: &TurnId) -> Result<&mut ProjectedTurn> {
        let index = self
            .turn_index
            .get(turn_id)
            .copied()
            .ok_or_else(|| ContextError::Invalid("Fact references an unknown turn".into()))?;
        self.turns
            .get_mut(index)
            .ok_or_else(|| ContextError::Invalid("turn index is corrupt".into()))
    }
}

fn assistant_message(
    content: Vec<ContentBlock>,
    replay: Option<&rsi_ai_protocol::ProviderExtension>,
) -> Result<Message> {
    let last_reasoning = content
        .iter()
        .rposition(|block| matches!(block, ContentBlock::Reasoning { .. }));
    let content = content
        .into_iter()
        .enumerate()
        .map(|(index, block)| match block {
            ContentBlock::Text { text } => MessageContent::Text { text },
            ContentBlock::Reasoning { text } => MessageContent::Reasoning {
                text,
                evidence: (Some(index) == last_reasoning)
                    .then(|| replay.cloned())
                    .flatten(),
            },
            ContentBlock::ToolCall(call) => MessageContent::ToolCall(call),
        })
        .collect();
    Message::assistant(content).map_err(|error| ContextError::Invalid(error.to_string()))
}

fn tool_message(call_id: &str, result: &ToolResult) -> Result<Message> {
    let mut content = Vec::new();
    for item in &result.content {
        match item {
            ToolContent::Text { text } => {
                content.push(MessageContent::Text { text: text.clone() });
            }
            ToolContent::Image { media } => {
                content.push(MessageContent::Image(media_descriptor(media)?));
            }
        }
    }
    if content.is_empty() {
        content.push(MessageContent::Text {
            text: serde_json::to_string(&result.value)
                .map_err(|error| ContextError::Invalid(error.to_string()))?,
        });
    }
    Message::tool_result(call_id, content, result.is_error)
        .map_err(|error| ContextError::Invalid(error.to_string()))
}

fn media_descriptor(media: &rsi_media_protocol::MediaRef) -> Result<MediaDescriptor> {
    MediaDescriptor::new(
        MediaKind::Image,
        media.mime.clone(),
        media.bytes,
        media.id.as_str(),
    )
    .and_then(|descriptor| descriptor.with_image_dimensions(media.width, media.height))
    .map_err(|error| ContextError::Invalid(error.to_string()))
}

fn encoded_message_bytes(message: &Message) -> Result<usize> {
    serde_json::to_vec(message)
        .map(|encoded| encoded.len())
        .map_err(|error| ContextError::Invalid(error.to_string()))
}

fn encoded_array_bytes(items: usize, item_bytes: usize) -> Result<usize> {
    let separators = items.saturating_sub(1);
    item_bytes
        .checked_add(separators)
        .and_then(|bytes| bytes.checked_add(2))
        .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))
}

/// Closed context projection failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContextError {
    /// Fact history or requested limits are invalid.
    #[error("invalid Agent context: {0}")]
    Invalid(String),
    /// Current nonterminal context cannot fit without splitting an active turn.
    #[error("Agent context exceeds its limits after all complete turns were compacted")]
    TooLarge,
}

/// Context result.
pub type Result<T> = std::result::Result<T, ContextError>;

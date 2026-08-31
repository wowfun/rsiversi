//! Incremental Fact-to-Language projection with complete-turn compaction.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use rsi_agent_session_protocol::{
    EMPTY_FACT_PREFIX_DIGEST, EffectId, SessionFact, SessionFactBody, SessionHeader, TurnId,
    advance_fact_prefix_digest,
};
use rsi_ai_protocol::{
    ContentBlock, LanguageAssembler, LanguageAssemblyError, LanguageRequest, Message,
    MessageContent, ToolChoice,
};
use rsi_media_protocol::{MediaDescriptor, MediaKind};
use rsi_tools_protocol::{ToolContent, ToolDefinition, ToolResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Borrow;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use thiserror::Error;

/// Default maximum projected Language messages.
pub const DEFAULT_CONTEXT_MESSAGES: usize = 256;
/// Default maximum canonical encoded message bytes.
pub const DEFAULT_CONTEXT_BYTES: usize = 8 * 1024 * 1024;
/// Absolute projected message bound.
pub const MAXIMUM_CONTEXT_MESSAGES: usize = 4_096;
/// Absolute projected byte bound.
pub const MAXIMUM_CONTEXT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum encoded Context-owned checkpoint bytes.
pub const MAXIMUM_CONTEXT_CHECKPOINT_BYTES: usize =
    rsi_agent_session_protocol::MAXIMUM_CONTEXT_CHECKPOINT_BYTES;
const CONTEXT_CHECKPOINT_VERSION: u32 = 3;
const CHECKPOINT_BINDING_DOMAIN: &[u8] = b"rsi-agent-context-checkpoint-v3\0";
const CHECKPOINT_MAGIC: &[u8] = b"rsi-agent-context-checkpoint-v3\0";

/// Explicit compaction limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    system_message: Option<Message>,
    system_message_bytes: usize,
    through_seq: u64,
    fact_prefix_digest: [u8; 32],
    checkpointable_prefix: bool,
    omitted_turns: usize,
    retention_limits: Option<ContextLimits>,
    turns: VecDeque<ProjectedTurn>,
    base_ordinal: usize,
    turn_index: BTreeMap<TurnId, usize>,
    assemblers: BTreeMap<EffectId, ActiveAssembler>,
    retained_messages: usize,
    retained_message_bytes: usize,
}

#[derive(Debug)]
struct ProjectedTurn {
    id: TurnId,
    messages: Vec<Message>,
    message_bytes: usize,
    terminal: bool,
}

#[derive(Debug)]
struct ActiveAssembler {
    turn_id: TurnId,
    assembler: LanguageAssembler,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextCheckpointPayloadV3 {
    version: u32,
    header_fingerprint: String,
    through_seq: u64,
    fact_prefix_sha256: String,
    omitted_turns: usize,
    retention_limits: ContextLimits,
    turns: Vec<CheckpointTurn>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointTurn {
    id: TurnId,
    messages: Vec<Message>,
    terminal: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ContextCheckpointPayloadRefV3<'a> {
    version: u32,
    header_fingerprint: &'a str,
    through_seq: u64,
    fact_prefix_sha256: &'a str,
    omitted_turns: usize,
    retention_limits: ContextLimits,
    turns: Vec<CheckpointTurnRef<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CheckpointTurnRef<'a> {
    id: &'a TurnId,
    messages: &'a [Message],
    terminal: bool,
}

impl ContextFold {
    /// Starts an empty projection for one immutable session.
    pub fn new(header: SessionHeader) -> Result<Self> {
        header
            .validate()
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        let system_message = if header.profile().system_prompt().is_empty() {
            None
        } else {
            Some(
                Message::system_text(header.profile().system_prompt())
                    .map_err(|error| ContextError::Invalid(error.to_string()))?,
            )
        };
        let system_message_bytes = system_message
            .as_ref()
            .map(encoded_message_bytes)
            .transpose()?
            .unwrap_or(0);
        Ok(Self {
            header,
            system_message,
            system_message_bytes,
            through_seq: 0,
            fact_prefix_digest: EMPTY_FACT_PREFIX_DIGEST,
            checkpointable_prefix: true,
            omitted_turns: 0,
            retention_limits: None,
            turns: VecDeque::new(),
            base_ordinal: 0,
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

    /// Returns the lowercase SHA-256 chain binding the exact applied Fact prefix.
    pub fn fact_prefix_sha256(&self) -> String {
        hex::encode(self.fact_prefix_digest)
    }

    /// Encodes a versioned checkpoint for an exact prefix without an active assembler.
    pub fn checkpoint_bytes(&self) -> Result<Arc<[u8]>> {
        let retention_limits = self.retention_limits.ok_or_else(|| {
            ContextError::Invalid("checkpoint requires explicit retention limits".into())
        })?;
        if self.through_seq == 0 || !self.checkpointable_prefix || !self.assemblers.is_empty() {
            return Err(ContextError::Invalid(
                "checkpoint requires a nonempty exact prefix without an active assembler".into(),
            ));
        }
        let header_fingerprint = self
            .header
            .fingerprint()
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        let fact_prefix_sha256 = self.fact_prefix_sha256();
        let payload = ContextCheckpointPayloadRefV3 {
            version: CONTEXT_CHECKPOINT_VERSION,
            header_fingerprint: &header_fingerprint,
            through_seq: self.through_seq,
            fact_prefix_sha256: &fact_prefix_sha256,
            omitted_turns: self.omitted_turns,
            retention_limits,
            turns: self
                .turns
                .iter()
                .map(|turn| CheckpointTurnRef {
                    id: &turn.id,
                    messages: &turn.messages,
                    terminal: turn.terminal,
                })
                .collect(),
        };
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        let mut digest = Sha256::new();
        digest.update(CHECKPOINT_BINDING_DOMAIN);
        digest.update(&payload_bytes);
        let binding: [u8; 32] = digest.finalize().into();
        let mut bytes =
            Vec::with_capacity(CHECKPOINT_MAGIC.len() + binding.len() + payload_bytes.len());
        bytes.extend_from_slice(CHECKPOINT_MAGIC);
        bytes.extend_from_slice(&binding);
        bytes.extend_from_slice(&payload_bytes);
        if bytes.is_empty() || bytes.len() > MAXIMUM_CONTEXT_CHECKPOINT_BYTES {
            return Err(ContextError::Invalid(
                "encoded checkpoint exceeds its absolute byte bound".into(),
            ));
        }
        Ok(Arc::from(bytes))
    }

    /// Restores a checkpoint only when its schema, header, and limits match.
    pub fn from_checkpoint(
        header: SessionHeader,
        limits: ContextLimits,
        bytes: &[u8],
    ) -> Result<Self> {
        header
            .validate()
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        ContextLimits::new(limits.max_messages, limits.max_bytes)?;
        if bytes.is_empty() || bytes.len() > MAXIMUM_CONTEXT_CHECKPOINT_BYTES {
            return Err(ContextError::Invalid(
                "checkpoint bytes are empty or exceed their absolute bound".into(),
            ));
        }
        let Some(envelope) = bytes.strip_prefix(CHECKPOINT_MAGIC) else {
            return Err(ContextError::Invalid(
                "checkpoint has the wrong format version".into(),
            ));
        };
        let Some((binding, payload_bytes)) = envelope.split_at_checked(32) else {
            return Err(ContextError::Invalid(
                "checkpoint envelope is truncated".into(),
            ));
        };
        let mut digest = Sha256::new();
        digest.update(CHECKPOINT_BINDING_DOMAIN);
        digest.update(payload_bytes);
        if binding != digest.finalize().as_slice() {
            return Err(ContextError::Invalid(
                "checkpoint binding does not match its retained projection".into(),
            ));
        }
        let checkpoint: ContextCheckpointPayloadV3 = serde_json::from_slice(payload_bytes)
            .map_err(|error| ContextError::Invalid(format!("invalid checkpoint: {error}")))?;
        let fact_prefix_digest = decode_sha256(
            "checkpoint Fact-prefix digest",
            &checkpoint.fact_prefix_sha256,
        )?;
        if checkpoint.version != CONTEXT_CHECKPOINT_VERSION
            || checkpoint.through_seq == 0
            || checkpoint.retention_limits != limits
            || checkpoint.header_fingerprint
                != header
                    .fingerprint()
                    .map_err(|error| ContextError::Invalid(error.to_string()))?
        {
            return Err(ContextError::Invalid(
                "checkpoint version, header, cursor, or limits do not match".into(),
            ));
        }
        let mut fold = Self::with_limits(header, limits)?;
        fold.through_seq = checkpoint.through_seq;
        fold.fact_prefix_digest = fact_prefix_digest;
        fold.checkpointable_prefix = true;
        fold.omitted_turns = checkpoint.omitted_turns;
        fold.base_ordinal = checkpoint.omitted_turns;
        for turn in checkpoint.turns {
            if fold.turn_index.contains_key(&turn.id) || turn.messages.is_empty() {
                return Err(ContextError::Invalid(
                    "checkpoint contains duplicate or empty turns".into(),
                ));
            }
            let mut message_bytes = 0_usize;
            for message in &turn.messages {
                message
                    .validate()
                    .map_err(|error| ContextError::Invalid(error.to_string()))?;
                message_bytes = message_bytes
                    .checked_add(encoded_message_bytes(message)?)
                    .ok_or_else(|| {
                        ContextError::Invalid("checkpoint message bytes overflowed".into())
                    })?;
            }
            let absolute = fold
                .base_ordinal
                .checked_add(fold.turns.len())
                .ok_or_else(|| ContextError::Invalid("turn ordinal overflowed".into()))?;
            fold.retained_messages = fold
                .retained_messages
                .checked_add(turn.messages.len())
                .ok_or_else(|| ContextError::Invalid("message count overflowed".into()))?;
            fold.retained_message_bytes = fold
                .retained_message_bytes
                .checked_add(message_bytes)
                .ok_or_else(|| ContextError::Invalid("message bytes overflowed".into()))?;
            fold.turn_index.insert(turn.id.clone(), absolute);
            fold.turns.push_back(ProjectedTurn {
                id: turn.id,
                messages: turn.messages,
                message_bytes,
                terminal: turn.terminal,
            });
        }
        if fold.retained_messages > MAXIMUM_CONTEXT_MESSAGES
            || fold.retained_message_bytes > MAXIMUM_CONTEXT_BYTES
        {
            return Err(ContextError::Invalid(
                "checkpoint retained projection exceeds absolute bounds".into(),
            ));
        }
        fold.compact_retained()?;
        Ok(fold)
    }

    /// Applies an exact contiguous suffix once.
    pub fn apply<T>(&mut self, facts: &[T]) -> Result<()>
    where
        T: Borrow<SessionFact>,
    {
        let mut expected = self
            .through_seq
            .checked_add(1)
            .ok_or_else(|| ContextError::Invalid("Fact sequence exhausted".into()))?;
        for fact in facts {
            let fact = fact.borrow();
            if fact.seq() != expected {
                return Err(ContextError::Invalid(format!(
                    "context expected Fact {expected}, got {}",
                    fact.seq()
                )));
            }
            fact.validate()
                .map_err(|error| ContextError::Invalid(error.to_string()))?;
            let next_digest = advance_fact_prefix(self.fact_prefix_digest, fact)?;
            self.apply_body(fact.body())?;
            self.through_seq = fact.seq();
            self.fact_prefix_digest = next_digest;
            self.compact_retained()?;
            expected = expected
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("Fact sequence exhausted".into()))?;
        }
        Ok(())
    }

    /// Applies visible Facts while advancing across claim-hidden sequence holes.
    pub fn apply_page<T>(&mut self, facts: &[T], through_seq: u64) -> Result<()>
    where
        T: Borrow<SessionFact>,
    {
        if through_seq < self.through_seq {
            return Err(ContextError::Invalid(
                "claim page watermark moved backwards".into(),
            ));
        }
        let mut previous = self.through_seq;
        for fact in facts {
            let fact = fact.borrow();
            if fact.seq() <= previous || fact.seq() > through_seq {
                return Err(ContextError::Invalid(
                    "claim page Facts are not increasing within its watermark".into(),
                ));
            }
            fact.validate()
                .map_err(|error| ContextError::Invalid(error.to_string()))?;
            if fact.seq() != previous.saturating_add(1) {
                self.checkpointable_prefix = false;
            }
            let next_digest = advance_fact_prefix(self.fact_prefix_digest, fact)?;
            self.apply_body(fact.body())?;
            self.fact_prefix_digest = next_digest;
            previous = fact.seq();
            self.compact_retained()?;
        }
        if through_seq != previous {
            self.checkpointable_prefix = false;
        }
        self.through_seq = through_seq;
        Ok(())
    }

    /// Projects bounded messages, dropping only complete oldest turns.
    pub fn project(&self, limits: ContextLimits) -> Result<ModelContext> {
        ContextLimits::new(limits.max_messages, limits.max_bytes)?;
        let mut retained_messages = usize::from(self.system_message.is_some())
            .checked_add(self.retained_messages)
            .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
        let mut retained_message_bytes = self
            .system_message_bytes
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
                messages.extend(self.system_message.iter().cloned());
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
            if self.turns.front().is_none_or(|turn| !turn.terminal) {
                break;
            }
            let removed = self
                .turns
                .pop_front()
                .expect("terminal front was observed above");
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
            self.turn_index.remove(&removed.id);
            self.base_ordinal = self
                .base_ordinal
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("turn ordinal overflowed".into()))?;
        }
        Ok(())
    }

    fn retained_shape_exceeds(&self, limits: ContextLimits) -> Result<bool> {
        let system_messages = usize::from(self.system_message.is_some());
        let count = system_messages
            .checked_add(usize::from(self.omitted_turns > 0))
            .and_then(|count| count.checked_add(self.retained_messages))
            .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
        let mut bytes = self.system_message_bytes;
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
            | SessionFactBody::BudgetExhausted { .. }
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
        let index = self.relative_index(index)?;
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
        let index = self
            .base_ordinal
            .checked_add(self.turns.len())
            .ok_or_else(|| ContextError::Invalid("turn ordinal overflowed".into()))?;
        self.turns.push_back(ProjectedTurn {
            id: turn_id.clone(),
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
        let index = self.relative_index(index)?;
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
        let index = self.relative_index(index)?;
        self.turns
            .get_mut(index)
            .ok_or_else(|| ContextError::Invalid("turn index is corrupt".into()))
    }

    fn relative_index(&self, absolute: usize) -> Result<usize> {
        absolute
            .checked_sub(self.base_ordinal)
            .filter(|index| *index < self.turns.len())
            .ok_or_else(|| ContextError::Invalid("turn index is corrupt".into()))
    }
}

fn advance_fact_prefix(previous: [u8; 32], fact: &SessionFact) -> Result<[u8; 32]> {
    advance_fact_prefix_digest(previous, fact)
        .map_err(|error| ContextError::Invalid(error.to_string()))
}

fn decode_sha256(name: &str, encoded: &str) -> Result<[u8; 32]> {
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ContextError::Invalid(format!(
            "{name} must be lowercase SHA-256"
        )));
    }
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(encoded, &mut digest)
        .map_err(|_| ContextError::Invalid(format!("{name} is invalid")))?;
    Ok(digest)
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

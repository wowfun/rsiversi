//! Incremental Fact-to-Language projection with complete-turn compaction.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod budget;
mod builder;
pub use budget::{ContextBudget, ContextBudgetContract, ContextBudgetFactory, ContextCredit};
mod compaction;
mod outcomes;
mod pruning;
pub use compaction::{PlannedCompaction, validate_summary_output};
mod default_provider;
mod emission;

pub use builder::{
    ContextBuilderIdentity, ContextInit, ContextPage, ContextPosition, ModelContextBuilder,
    ModelContextBuilderContract, ModelContextCursor, ModelContextState,
};
pub use default_provider::{DefaultContextBuilder, DefaultContextBuilderFactory};

use rsi_agent_session_protocol::{
    AgentMessageContent, EMPTY_FACT_PREFIX_DIGEST, EffectId, InputMessageSource, SessionFact,
    SessionFactBody, SessionHeader, TurnId, advance_fact_prefix_digest,
};
use rsi_ai_protocol::{
    ContentBlock, LanguageAssembler, LanguageAssemblyError, LanguageEvent, LanguageRequest,
    LanguageRequestOptions, Message, MessageContent,
};
use rsi_media_protocol::{MediaDescriptor, MediaKind};
use rsi_tools_protocol::{ToolContent, ToolResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::{Borrow, Cow};
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
const CONTEXT_CHECKPOINT_VERSION: u32 = 10;
const CHECKPOINT_BINDING_DOMAIN: &[u8] = b"rsi-agent-context-checkpoint-v10\0";
const CHECKPOINT_MAGIC: &[u8] = b"rsi-agent-context-checkpoint-v10\0";

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
#[derive(Debug)]
pub struct ModelContext {
    _credit: Arc<ContextCredit>,
    messages: Vec<Message>,
    /// Number of complete oldest turns omitted as one unit.
    pub omitted_turns: usize,
    /// Highest applied Fact sequence.
    pub through_seq: u64,
}

struct ProjectedMessages {
    messages: Vec<Message>,
    omitted_turns: usize,
    encoded_bytes: usize,
}

impl PartialEq for ModelContext {
    fn eq(&self, other: &Self) -> bool {
        self.messages == other.messages
            && self.omitted_turns == other.omitted_turns
            && self.through_seq == other.through_seq
    }
}

impl ModelContext {
    /// Borrows provider-neutral ordered messages without separating their admission.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// Incremental fold over one immutable session header and its Facts.
#[derive(Debug)]
pub struct ContextFold {
    header: SessionHeader,
    budget: ContextBudget,
    credit: ContextCredit,
    header_weight: usize,
    metadata_weights: BTreeMap<TurnId, usize>,
    metadata_bytes: usize,
    global_metadata_bytes: usize,
    semantic: Option<compaction::SemanticState>,
    system_message: Option<Message>,
    system_message_bytes: usize,
    through_seq: u64,
    seed_through_seq: u64,
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
    ingestion_failed: bool,
    #[cfg(test)]
    metadata_recounts: std::cell::Cell<usize>,
}

#[derive(Debug, Serialize)]
struct ProjectedTurn {
    id: TurnId,
    messages: Vec<Message>,
    message_bytes: usize,
    terminal: bool,
    batches: outcomes::Batches,
}

#[derive(Debug)]
struct ActiveAssembler {
    binding_bytes: usize,
    purpose: rsi_agent_session_protocol::ModelPurpose,
    eligible_summary: bool,
    model: rsi_ai_protocol::ModelRef,
    turn_id: TurnId,
    assembler: LanguageAssembler,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextCheckpointPayload {
    version: u32,
    header_fingerprint: String,
    through_seq: u64,
    fact_prefix_sha256: String,
    omitted_turns: usize,
    retention_limits: ContextLimits,
    turns: Vec<CheckpointTurn>,
    semantic: Option<compaction::SemanticState>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointTurn {
    id: TurnId,
    messages: Vec<Message>,
    terminal: bool,
    batches: outcomes::Batches,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ContextCheckpointPayloadRef<'a> {
    version: u32,
    header_fingerprint: &'a str,
    through_seq: u64,
    fact_prefix_sha256: &'a str,
    omitted_turns: usize,
    retention_limits: ContextLimits,
    turns: Vec<CheckpointTurnRef<'a>>,
    semantic: Option<&'a compaction::SemanticState>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CheckpointTurnRef<'a> {
    id: &'a TurnId,
    messages: &'a [Message],
    terminal: bool,
    batches: &'a outcomes::Batches,
}

impl ContextFold {
    /// Starts an empty projection for one immutable session.
    pub fn new(header: SessionHeader, budget: ContextBudget) -> Result<Self> {
        // Cursor Header and derived instruction/message copy; README defines the separate wrapper charge.
        let header_weight = budget::weight(&header)?;
        let credit = budget.reserve(header_weight.checked_mul(2).ok_or(ContextError::Capacity)?)?;
        header
            .validate()
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        let mut instructions = header.settings().system_prompt().to_owned();
        if let Some(persona) = header
            .delegation_policy()
            .and_then(|policy| policy.persona())
        {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(persona);
        }
        let system_message = if instructions.is_empty() {
            None
        } else {
            Some(
                Message::system_text(instructions)
                    .map_err(|error| ContextError::Invalid(error.to_string()))?,
            )
        };
        let system_message_bytes = system_message
            .as_ref()
            .map(encoded_message_bytes)
            .transpose()?
            .unwrap_or(0);
        let seed_through_seq = header
            .fork_origin()
            .map_or(0, |origin| origin.resolved_after_seq);
        let mut state = Self {
            header,
            budget,
            credit,
            header_weight,
            metadata_weights: BTreeMap::new(),
            metadata_bytes: 0,
            global_metadata_bytes: 0,
            semantic: None,
            system_message,
            system_message_bytes,
            through_seq: 0,
            seed_through_seq,
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
            ingestion_failed: false,
            #[cfg(test)]
            metadata_recounts: std::cell::Cell::new(0),
        };
        state.reaccount()?;
        Ok(state)
    }

    /// Starts an incremental projection that discards complete old turns as it folds.
    pub fn with_limits(
        header: SessionHeader,
        limits: ContextLimits,
        budget: ContextBudget,
    ) -> Result<Self> {
        ContextLimits::new(limits.max_messages, limits.max_bytes)?;
        let mut fold = Self::new(header, budget)?;
        fold.retention_limits = Some(limits);
        Ok(fold)
    }

    fn retained_weight(&self) -> Result<usize> {
        // Message canonical sizes are established at their insertion boundary. Counting
        // them again on every Fact would rescan the complete retained text quadratically.
        let mut size = self
            .header_weight
            .checked_add(self.metadata_bytes)
            .and_then(|n| n.checked_add(self.global_metadata_bytes))
            .ok_or(ContextError::Capacity)?
            .checked_add(self.system_message_bytes)
            .and_then(|n| n.checked_add(self.retained_message_bytes))
            .ok_or(ContextError::Capacity)?;
        for assembler in self.assemblers.values() {
            size = size
                .checked_add(assembler.binding_bytes)
                .and_then(|n| n.checked_add(assembler.assembler.retained_encoded_weight()))
                .ok_or(ContextError::Capacity)?;
        }
        Ok(size)
    }
    fn reaccount(&mut self) -> Result<()> {
        self.metadata_weights.clear();
        self.metadata_bytes = 0;
        self.global_metadata_bytes = self
            .semantic
            .as_ref()
            .map_or(Ok(0), compaction::SemanticState::global_weight)?;
        for turn in &self.turns {
            let bytes = self.turn_metadata_weight(turn)?;
            self.metadata_bytes = self
                .metadata_bytes
                .checked_add(bytes)
                .ok_or(ContextError::Capacity)?;
            self.metadata_weights.insert(turn.id.clone(), bytes);
        }
        self.credit.resize(self.retained_weight()?)
    }
    fn turn_metadata_weight(&self, turn: &ProjectedTurn) -> Result<usize> {
        #[cfg(test)]
        self.metadata_recounts.set(self.metadata_recounts.get() + 1);
        let identity = budget::weight(&turn.id)?
            .checked_mul(6)
            .ok_or(ContextError::Capacity)?;
        let semantic = self
            .semantic
            .as_ref()
            .map_or(Ok(0), |state| state.turn_weight(&turn.id))?;
        budget::weight(&(turn.terminal, usize::MAX))?
            .checked_add(turn.batches.weight()?)
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or(ContextError::Capacity)?
            .checked_add(identity)
            .and_then(|n| n.checked_add(semantic))
            .ok_or(ContextError::Capacity)
    }
    fn reaccount_fact(&mut self, fact: &SessionFact, changed: Vec<TurnId>) -> Result<()> {
        if matches!(
            fact.body(),
            SessionFactBody::ModelEvent {
                event: LanguageEvent::ContentDelta { .. },
                ..
            }
        ) {
            // Deltas change only assembler strings and fixed-width source digests.
            // Source sequence numbers are accounted at their maximum width.
            return self.credit.resize(self.retained_weight()?);
        }
        if matches!(
            fact.body(),
            SessionFactBody::ModelEvent {
                event: LanguageEvent::Finished { .. },
                purpose: rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction,
                ..
            }
        ) {
            return self.reaccount();
        }
        let changed: std::collections::BTreeSet<_> = changed
            .into_iter()
            .chain(std::iter::once(fact.body().turn_id().clone()))
            .collect();
        for id in changed {
            self.reaccount_turn(&id)?;
        }
        if matches!(
            fact.body(),
            SessionFactBody::ModelEvent {
                event: LanguageEvent::Finished { .. },
                ..
            }
        ) {
            self.global_metadata_bytes = self
                .semantic
                .as_ref()
                .map_or(Ok(0), compaction::SemanticState::global_weight)?;
        }
        self.credit.resize(self.retained_weight()?)
    }
    fn reaccount_turn(&mut self, id: &TurnId) -> Result<()> {
        let next = self
            .turn_index
            .get(id)
            .map(|ordinal| self.relative_index(*ordinal))
            .transpose()?
            .map(|index| self.turn_metadata_weight(&self.turns[index]))
            .transpose()?;
        if let Some(previous) = self.metadata_weights.remove(id) {
            self.metadata_bytes = self
                .metadata_bytes
                .checked_sub(previous)
                .ok_or(ContextError::Capacity)?;
        }
        if let Some(next) = next {
            self.metadata_bytes = self
                .metadata_bytes
                .checked_add(next)
                .ok_or(ContextError::Capacity)?;
            self.metadata_weights.insert(id.clone(), next);
        }
        Ok(())
    }
    fn admit_fact(&mut self, fact: &SessionFact) -> Result<()> {
        // Summary installation may retain a replacement alongside the complete old view.
        // Reserve policy headroom for projected/semantic copies and JSON quoting;
        // this encoded-weight multiplier is not an allocator bound.
        let growth = fact
            .encoded_len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(self.credit.bytes().checked_mul(2)?))
            .ok_or(ContextError::Capacity)?;
        self.credit.resize(
            self.credit
                .bytes()
                .checked_add(growth)
                .ok_or(ContextError::Capacity)?,
        )
    }
    fn ensure_usable(&self) -> Result<()> {
        if self.ingestion_failed {
            return Err(ContextError::Invalid(
                "Context cursor was invalidated by failed Fact ingestion; rebuild from authoritative Facts".into(),
            ));
        }
        Ok(())
    }
    fn apply_admitted_fact(
        &mut self,
        fact: &SessionFact,
        source: &rsi_agent_session_protocol::SessionId,
    ) -> Result<()> {
        self.admit_fact(fact)?;
        let result = (|| {
            self.apply_body(fact.body(), fact.seq())?;
            let changed = self.record_semantic_fact(source, fact)?;
            self.compact_retained()?;
            self.reaccount_fact(fact, changed)
        })();
        if result.is_err() {
            // Mutations can precede a refusal. Drop their ownership before releasing
            // credit rather than pretending that the old prefix was rolled back.
            self.ingestion_failed = true;
            self.checkpointable_prefix = false;
            self.turns.clear();
            self.turn_index.clear();
            self.assemblers.clear();
            self.semantic = None;
            self.system_message = None;
            self.system_message_bytes = 0;
            self.metadata_weights.clear();
            self.metadata_bytes = 0;
            self.global_metadata_bytes = 0;
            self.retained_messages = 0;
            self.retained_message_bytes = 0;
            self.credit
                .resize(self.header_weight)
                .expect("the immutable Header was already admitted");
        }
        result
    }
    pub(crate) fn projection_credit(&self, additional: usize) -> Result<ContextCredit> {
        self.ensure_usable()?;
        self.budget.reserve(
            self.credit
                .bytes()
                .checked_mul(4)
                .and_then(|n| n.checked_add(additional))
                .ok_or(ContextError::Capacity)?,
        )
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
    pub fn checkpoint_bytes(&self) -> Result<rsi_api_protocol::RetainedBytes> {
        self.ensure_usable()?;
        let retention_limits = self.retention_limits.ok_or_else(|| {
            ContextError::Invalid("checkpoint requires explicit retention limits".into())
        })?;
        if self.through_seq == 0
            || !self.checkpointable_prefix
            || !self.assemblers.is_empty()
            || self
                .turns
                .iter()
                .any(|turn| turn.messages.is_empty() && !turn.terminal)
        {
            return Err(ContextError::Invalid(
                "checkpoint requires a nonempty exact prefix without an active assembler or empty turn"
                    .into(),
            ));
        }
        let header_fingerprint = self
            .header
            .fingerprint()
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        let fact_prefix_sha256 = self.fact_prefix_sha256();
        let payload = ContextCheckpointPayloadRef {
            version: CONTEXT_CHECKPOINT_VERSION,
            semantic: self.semantic.as_ref(),
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
                    batches: &turn.batches,
                })
                .collect(),
        };
        let prefix = CHECKPOINT_MAGIC.len() + 32;
        let size = prefix
            .checked_add(budget::weight(&payload)?)
            .ok_or(ContextError::Capacity)?;
        let credit = self.budget.reserve(size)?;
        let reservation = rsi_api_protocol::ByteReservation::from_retention(size, credit)
            .map_err(|e| ContextError::Invalid(e.to_string()))?;
        let mut writer = CheckpointWriter {
            bytes: Vec::with_capacity(size),
            limit: MAXIMUM_CONTEXT_CHECKPOINT_BYTES,
        };
        writer.bytes.extend_from_slice(CHECKPOINT_MAGIC);
        writer.bytes.resize(prefix, 0);
        serde_json::to_writer(&mut writer, &payload)
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        let mut bytes = writer.bytes;
        let mut digest = Sha256::new();
        digest.update(CHECKPOINT_BINDING_DOMAIN);
        digest.update(&bytes[prefix..]);
        bytes[CHECKPOINT_MAGIC.len()..prefix].copy_from_slice(&digest.finalize());
        reservation
            .retain_vec(bytes)
            .map_err(|e| ContextError::Invalid(e.to_string()))
    }

    /// Restores a checkpoint only when its schema, header, and limits match.
    pub fn from_checkpoint(
        header: SessionHeader,
        limits: ContextLimits,
        bytes: &[u8],
        budget: ContextBudget,
    ) -> Result<Self> {
        let _restore = budget.reserve(bytes.len().checked_mul(3).ok_or(ContextError::Capacity)?)?;
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
        let checkpoint: ContextCheckpointPayload = serde_json::from_slice(payload_bytes)
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
        let mut fold = Self::with_limits(header, limits, budget)?;
        fold.semantic = checkpoint.semantic;
        fold.through_seq = checkpoint.through_seq;
        fold.fact_prefix_digest = fact_prefix_digest;
        fold.checkpointable_prefix = true;
        fold.omitted_turns = checkpoint.omitted_turns;
        fold.base_ordinal = checkpoint.omitted_turns;
        fold.restore_checkpoint_turns(checkpoint.turns)?;
        fold.validate_semantic()?;
        fold.reaccount()?;
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

    fn restore_checkpoint_turns(&mut self, turns: Vec<CheckpointTurn>) -> Result<()> {
        for turn in turns {
            if self.turn_index.contains_key(&turn.id)
                || (turn.messages.is_empty() && !turn.terminal)
            {
                return Err(ContextError::Invalid(
                    "checkpoint contains duplicate, empty, or misaligned turns".into(),
                ));
            }
            outcomes::validate(&turn.batches, &turn.messages)?;
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
            let absolute = self
                .base_ordinal
                .checked_add(self.turns.len())
                .ok_or_else(|| ContextError::Invalid("turn ordinal overflowed".into()))?;
            self.retained_messages = self
                .retained_messages
                .checked_add(turn.messages.len())
                .ok_or_else(|| ContextError::Invalid("message count overflowed".into()))?;
            self.retained_message_bytes = self
                .retained_message_bytes
                .checked_add(message_bytes)
                .ok_or_else(|| ContextError::Invalid("message bytes overflowed".into()))?;
            self.turn_index.insert(turn.id.clone(), absolute);
            self.turns.push_back(ProjectedTurn {
                id: turn.id,
                messages: turn.messages,
                message_bytes,
                terminal: turn.terminal,
                batches: turn.batches,
            });
        }
        Ok(())
    }

    /// Applies an exact contiguous suffix once.
    pub fn apply<T>(&mut self, facts: &[T]) -> Result<()>
    where
        T: Borrow<SessionFact>,
    {
        self.ensure_child_facts_may_begin()?;
        let mut expected = self
            .through_seq
            .checked_add(1)
            .ok_or_else(|| ContextError::Invalid("Fact sequence exhausted".into()))?;
        let session = self.header.session_id().clone();
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
            self.apply_admitted_fact(fact, &session)?;
            self.through_seq = fact.seq();
            self.fact_prefix_digest = next_digest;
            expected = expected
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("Fact sequence exhausted".into()))?;
        }
        Ok(())
    }

    /// Applies inherited parent Facts without advancing the child session cursor.
    ///
    /// Seed pages must exactly continue the Header's inherited interval across
    /// page boundaries and may only be applied before any child Fact. Call
    /// [`Self::finish_seed`] after the last page.
    pub fn apply_seed_page<T>(&mut self, facts: &[T]) -> Result<()>
    where
        T: Borrow<SessionFact>,
    {
        self.ensure_usable()?;
        if self.through_seq != 0 {
            return Err(ContextError::Invalid(
                "fork seed cannot follow child session Facts".into(),
            ));
        }
        let origin = self
            .header
            .fork_origin()
            .ok_or_else(|| ContextError::Invalid("fork seed requires fork lineage".into()))?;
        let terminal_seq = origin.resolved_terminal_seq;
        let parent_session = origin.parent_session_id.clone();
        let mut expected = self
            .seed_through_seq
            .checked_add(1)
            .ok_or_else(|| ContextError::Invalid("parent Fact sequence exhausted".into()))?;
        for fact in facts {
            let fact = fact.borrow();
            if fact.seq() != expected || fact.seq() > terminal_seq {
                return Err(ContextError::Invalid(format!(
                    "fork seed expected parent Fact {expected}, got {} within terminal {terminal_seq}",
                    fact.seq()
                )));
            }
            fact.validate()
                .map_err(|error| ContextError::Invalid(error.to_string()))?;
            self.apply_admitted_fact(fact, &parent_session)?;
            self.seed_through_seq = fact.seq();
            expected = expected
                .checked_add(1)
                .ok_or_else(|| ContextError::Invalid("parent Fact sequence exhausted".into()))?;
        }
        Ok(())
    }

    /// Closes seed loading only after exact interval coverage and balanced terminal Turns.
    pub fn finish_seed(&self) -> Result<()> {
        self.ensure_usable()?;
        if self.through_seq != 0 {
            return Err(ContextError::Invalid(
                "fork seed cannot follow child session Facts".into(),
            ));
        }
        self.validate_complete_seed()
    }

    fn validate_complete_seed(&self) -> Result<()> {
        let terminal_seq = self
            .header
            .fork_origin()
            .ok_or_else(|| ContextError::Invalid("fork seed requires fork lineage".into()))?
            .resolved_terminal_seq;
        if self.seed_through_seq != terminal_seq {
            return Err(ContextError::Invalid(format!(
                "fork seed must cover the complete inherited interval through parent Fact {terminal_seq}"
            )));
        }
        if !self.assemblers.is_empty() || self.turns.iter().any(|turn| !turn.terminal) {
            return Err(ContextError::Invalid(
                "fork seed must contain only balanced completed turns".into(),
            ));
        }
        Ok(())
    }

    fn ensure_child_facts_may_begin(&self) -> Result<()> {
        self.ensure_usable()?;
        if self.through_seq == 0 && self.header.fork_origin().is_some() {
            self.validate_complete_seed()?;
        }
        Ok(())
    }

    /// Applies visible Facts while advancing across claim-hidden sequence holes.
    pub fn apply_page<T>(&mut self, facts: &[T], through_seq: u64) -> Result<()>
    where
        T: Borrow<SessionFact>,
    {
        self.ensure_child_facts_may_begin()?;
        if through_seq < self.through_seq {
            return Err(ContextError::Invalid(
                "claim page watermark moved backwards".into(),
            ));
        }
        let mut previous = self.through_seq;
        let session = self.header.session_id().clone();
        for fact in facts {
            let fact = fact.borrow();
            if fact.seq() <= previous || fact.seq() > through_seq {
                return Err(ContextError::Invalid(
                    "claim page Facts are not increasing within its watermark".into(),
                ));
            }
            fact.validate()
                .map_err(|error| ContextError::Invalid(error.to_string()))?;
            let next_digest = advance_fact_prefix(self.fact_prefix_digest, fact)?;
            self.apply_admitted_fact(fact, &session)?;
            if fact.seq() != previous.saturating_add(1) {
                self.checkpointable_prefix = false;
            }
            self.through_seq = fact.seq();
            self.fact_prefix_digest = next_digest;
            previous = fact.seq();
        }
        if through_seq != previous {
            self.checkpointable_prefix = false;
        }
        self.through_seq = through_seq;
        Ok(())
    }

    /// Projects bounded messages, dropping only complete oldest turns.
    pub fn project(&self, limits: ContextLimits) -> Result<ModelContext> {
        self.project_view(limits, false)
    }

    fn project_view(&self, limits: ContextLimits, provider_view: bool) -> Result<ModelContext> {
        let mut credit = self.projection_credit(0)?;
        let projected = self.projected_messages(limits, provider_view)?;
        credit.resize(projected.encoded_bytes)?;
        Ok(ModelContext {
            _credit: Arc::new(credit),
            messages: projected.messages,
            omitted_turns: projected.omitted_turns,
            through_seq: self.through_seq,
        })
    }

    fn projected_messages(
        &self,
        limits: ContextLimits,
        provider_view: bool,
    ) -> Result<ProjectedMessages> {
        self.ensure_usable()?;
        // Private callers hold projection credit throughout materialization.
        ContextLimits::new(limits.max_messages, limits.max_bytes)?;
        if self.semantic.is_some() {
            return self.semantic_projection(limits);
        }
        let normalized = provider_view
            .then(|| {
                self.turns
                    .iter()
                    .map(|turn| {
                        outcomes::normalize_with_size(
                            &turn.messages,
                            &turn.batches,
                            turn.terminal,
                            turn.message_bytes,
                        )
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let (retained_count, retained_bytes) = normalized.as_ref().map_or(
            (self.retained_messages, self.retained_message_bytes),
            |turns| {
                turns
                    .iter()
                    .fold((0, 0), |(count, bytes), (messages, size)| {
                        (count + messages.len(), bytes + size)
                    })
            },
        );
        let mut retained_messages = usize::from(self.system_message.is_some())
            .checked_add(retained_count)
            .ok_or_else(|| ContextError::Invalid("context message count overflowed".into()))?;
        let mut retained_message_bytes = self
            .system_message_bytes
            .checked_add(retained_bytes)
            .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        let mut omitted = self.omitted_turns;
        let mut skipped_retained = 0_usize;
        loop {
            let notice = (omitted > 0)
                .then(|| omission_message(omitted))
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
                if let Some(turns) = &normalized {
                    for (turn, _) in turns.iter().skip(skipped_retained) {
                        messages.extend(turn.iter().map(|message| message.as_ref().clone()));
                    }
                } else {
                    for turn in self.turns.iter().skip(skipped_retained) {
                        messages.extend(turn.messages.iter().cloned());
                    }
                }
                drop(normalized);
                return Ok(ProjectedMessages {
                    messages,
                    omitted_turns: omitted,
                    encoded_bytes: encoded_array_bytes(message_count, message_bytes)?,
                });
            }
            let Some(turn) = self.turns.get(skipped_retained) else {
                return Err(ContextError::TooLarge);
            };
            if !turn.terminal {
                return Err(ContextError::TooLarge);
            }
            let (removed_messages, removed_bytes) = normalized
                .as_ref()
                .map_or((turn.messages.len(), turn.message_bytes), |turns| {
                    (turns[skipped_retained].0.len(), turns[skipped_retained].1)
                });
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
        if self.semantic.is_some() {
            // Never evict source messages before semantic planning. The absolute
            // materialization ceiling still bounds a cold or adversarial history.
            if self.retained_messages > MAXIMUM_CONTEXT_MESSAGES
                || self.retained_message_bytes > MAXIMUM_CONTEXT_BYTES
            {
                return Err(ContextError::TooLarge);
            }
            return Ok(());
        }
        let Some(limits) = self.retention_limits else {
            return Ok(());
        };
        while self.retained_shape_exceeds(limits)? {
            if self.turns.front().is_none_or(|turn| !turn.terminal) {
                break;
            }
            self.remove_oldest_turn()?;
        }
        Ok(())
    }

    // Called only after observing a terminal front; no active Turn is truncated.
    fn remove_oldest_turn(&mut self) -> Result<()> {
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
        if let Some(previous) = self.metadata_weights.remove(&removed.id) {
            self.metadata_bytes = self
                .metadata_bytes
                .checked_sub(previous)
                .ok_or(ContextError::Capacity)?;
        }
        self.base_ordinal = self
            .base_ordinal
            .checked_add(1)
            .ok_or_else(|| ContextError::Invalid("turn ordinal overflowed".into()))?;
        Ok(())
    }

    fn admit_message(&mut self, bytes: usize) -> Result<()> {
        while self.retained_messages >= MAXIMUM_CONTEXT_MESSAGES
            || bytes > MAXIMUM_CONTEXT_BYTES.saturating_sub(self.retained_message_bytes)
        {
            if self.semantic.is_some()
                || self.retention_limits.is_none()
                || self.turns.front().is_none_or(|turn| !turn.terminal)
            {
                return Err(ContextError::TooLarge);
            }
            self.remove_oldest_turn()?;
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
                .checked_add(encoded_message_bytes(&omission_message(
                    self.omitted_turns,
                )?)?)
                .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        }
        bytes = bytes
            .checked_add(self.retained_message_bytes)
            .ok_or_else(|| ContextError::Invalid("context byte count overflowed".into()))?;
        Ok(count > limits.max_messages || encoded_array_bytes(count, bytes)? > limits.max_bytes)
    }

    /// Builds one provider-neutral Language request.
    ///
    /// V1 keeps the complete provider-neutral prefix. A provider replay token is
    /// not eligible for prefix elision until the AI seam exposes an exact,
    /// provider-I/O-free route/config/credential identity before request construction.
    pub fn request(
        &self,
        limits: ContextLimits,
        options: LanguageRequestOptions,
    ) -> Result<LanguageRequest> {
        let mut credit = self.projection_credit(options.encoded_weight())?;
        let messages = self.request_messages(limits, &options)?;
        // Projection proved the limits; removing provider-private blocks only shrinks it.
        let request = LanguageRequest::new_with_options(messages, options)
            .map_err(|error| ContextError::Invalid(error.to_string()))?;
        credit.resize(request.encoded_weight())?;
        Ok(request.with_retention(credit))
    }

    fn request_messages(
        &self,
        limits: ContextLimits,
        options: &LanguageRequestOptions,
    ) -> Result<Vec<Message>> {
        let limits = emission_limits(limits, options)?;
        without_unscoped_provider_state(self.projected_messages(limits, true)?.messages)
    }

    fn apply_body(&mut self, body: &SessionFactBody, seq: u64) -> Result<()> {
        let result = self.apply_body_inner(body, seq);
        if result.is_err() {
            // A failed assembler or ingestion may have consumed transient state.
            // It must never be cached as the preceding exact Fact prefix.
            self.checkpointable_prefix = false;
        }
        result
    }

    fn apply_body_inner(&mut self, body: &SessionFactBody, seq: u64) -> Result<()> {
        match body {
            SessionFactBody::TurnAccepted { turn_id, text, .. } => {
                let message = Message::user_text(text)
                    .map_err(|error| ContextError::Invalid(error.to_string()))?;
                self.insert_turn(turn_id, message)?;
            }
            SessionFactBody::MessageTurnAccepted { turn_id, .. } => {
                self.insert_empty_turn(turn_id)?;
            }
            SessionFactBody::InputMessageEntered {
                turn_id,
                source,
                content,
                ..
            } => {
                let message = input_message(source, content, seq)?;
                self.push_turn_message(turn_id, message)?;
            }
            SessionFactBody::ImageRequested {
                turn_id, request, ..
            } => {
                let message = Message::user_text(request.prompt())
                    .map_err(|error| ContextError::Invalid(error.to_string()))?;
                self.insert_turn(turn_id, message)?;
            }
            SessionFactBody::ModelIntent {
                evidence: _,
                price_quote: _,
                turn_id,
                effect_id,
                purpose,
                snapshot,
            } => {
                self.start_model(turn_id, effect_id, purpose, snapshot)?;
            }
            SessionFactBody::ModelEvent {
                turn_id,
                effect_id,
                event,
                purpose,
            } => self.apply_model_event(turn_id, effect_id, event, *purpose, seq)?,
            SessionFactBody::ToolCallsSuperseded { .. }
            | SessionFactBody::ToolIntent { .. }
            | SessionFactBody::ToolStarted { .. }
            | SessionFactBody::ToolRejected { .. }
            | SessionFactBody::ToolResult { .. } => self.apply_tool_outcome(body)?,
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
                // A terminal Turn cannot emit more events. Unfinished effects
                // remain visible as raw Facts but cannot poison the next cursor
                // boundary or install an uncompleted internal summary.
                self.assemblers
                    .retain(|_, active| &active.turn_id != turn_id);
            }
            SessionFactBody::CancelRequested { .. }
            | SessionFactBody::StepStarted { .. }
            | SessionFactBody::StepEnded { .. }
            | SessionFactBody::WorkspaceTouched { .. }
            | SessionFactBody::BudgetExhausted { .. }
            | SessionFactBody::ModelStarted { .. }
            | SessionFactBody::ImageIntent { .. }
            | SessionFactBody::ImageStarted { .. } => {}
        }
        Ok(())
    }

    fn start_model(
        &mut self,
        turn_id: &TurnId,
        effect_id: &EffectId,
        purpose: &rsi_agent_session_protocol::ModelPurpose,
        snapshot: &rsi_ai_protocol::PreparedCallSnapshot,
    ) -> Result<()> {
        self.require_live_turn(turn_id)?;
        let eligible_summary = match purpose {
            rsi_agent_session_protocol::ModelPurpose::Conversation => false,
            rsi_agent_session_protocol::ModelPurpose::ContextCompaction(plan) => {
                self.summary_eligible(plan)
            }
        };
        let model =
            rsi_ai_protocol::ModelRef::new(snapshot.deployment_id.clone(), snapshot.model.clone())
                .map_err(|error| ContextError::Invalid(error.to_string()))?;
        if self
            .assemblers
            .insert(
                effect_id.clone(),
                ActiveAssembler {
                    binding_bytes: budget::weight(&(effect_id, purpose, &model, turn_id))?,
                    purpose: purpose.clone(),
                    eligible_summary,
                    model,
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
        Ok(())
    }

    fn apply_model_event(
        &mut self,
        turn_id: &TurnId,
        effect_id: &EffectId,
        event: &rsi_ai_protocol::LanguageEvent,
        purpose: rsi_agent_session_protocol::ModelEventPurpose,
        seq: u64,
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
        if &active.turn_id != turn_id || active.purpose.event_purpose() != purpose {
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
                let internal = matches!(
                    active.purpose,
                    rsi_agent_session_protocol::ModelPurpose::ContextCompaction(_)
                );
                self.finish_semantic(
                    effect_id,
                    &active.purpose,
                    active.eligible_summary,
                    active.model,
                    seq,
                    &output,
                )?;
                if internal {
                    return Ok(());
                }
                let message = assistant_message(output.content, output.replay.as_ref())?;
                let turn = self.turn_mut(turn_id)?;
                let index = turn.messages.len();
                let batch = outcomes::prepare_batch(&turn.batches, index, effect_id, &message)?;
                self.push_turn_message_with(turn_id, message, |turn| {
                    if let Some(batch) = batch {
                        turn.batches.insert(index, batch);
                    }
                })?;
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
        self.admit_message(message_bytes)?;
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
            batches: outcomes::Batches::new(),
        });
        self.turn_index.insert(turn_id.clone(), index);
        self.retained_messages = retained_messages;
        self.retained_message_bytes = retained_message_bytes;
        Ok(())
    }

    fn insert_empty_turn(&mut self, turn_id: &TurnId) -> Result<()> {
        if self.turn_index.contains_key(turn_id) {
            return Err(ContextError::Invalid(
                "turn was accepted more than once".into(),
            ));
        }
        let index = self
            .base_ordinal
            .checked_add(self.turns.len())
            .ok_or_else(|| ContextError::Invalid("turn ordinal overflowed".into()))?;
        self.turns.push_back(ProjectedTurn {
            id: turn_id.clone(),
            messages: Vec::new(),
            message_bytes: 0,
            terminal: false,
            batches: outcomes::Batches::new(),
        });
        self.turn_index.insert(turn_id.clone(), index);
        Ok(())
    }

    fn push_turn_message(&mut self, turn_id: &TurnId, message: Message) -> Result<()> {
        self.push_turn_message_with(turn_id, message, |_| {})
    }

    // Validate provenance before entering this helper. Capacity admission may
    // evict complete older Turns; the live target and its indices stay stable.
    fn push_turn_message_with(
        &mut self,
        turn_id: &TurnId,
        message: Message,
        commit: impl FnOnce(&mut ProjectedTurn),
    ) -> Result<()> {
        let index = self
            .turn_index
            .get(turn_id)
            .copied()
            .ok_or_else(|| ContextError::Invalid("Fact references an unknown turn".into()))?;
        let message_bytes = encoded_message_bytes(&message)?;
        self.admit_message(message_bytes)?;
        let index = self.relative_index(index)?;
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
        commit(turn);
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

fn emission_limits(
    limits: ContextLimits,
    options: &LanguageRequestOptions,
) -> Result<ContextLimits> {
    ContextLimits::new(
        limits.max_messages.min(rsi_ai_protocol::MAX_MESSAGES),
        limits.max_bytes.min(options.message_byte_budget()),
    )
}

fn encoded_bytes<T: Serialize + ?Sized>(value: &T) -> Result<usize> {
    #[derive(Default)]
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("encoded size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value)
        .map_err(|error| ContextError::Invalid(error.to_string()))?;
    Ok(counter.0)
}

fn has_unscoped_provider_state(message: &Message) -> bool {
    message.role() == rsi_ai_protocol::MessageRole::Assistant
        && message
            .content()
            .iter()
            .any(|block| matches!(block, MessageContent::Reasoning { .. }))
}

fn without_unscoped_provider_message(
    message: Cow<'_, Message>,
) -> Result<Option<Cow<'_, Message>>> {
    if !has_unscoped_provider_state(&message) {
        return Ok(Some(message));
    }
    let content = message
        .content()
        .iter()
        .filter(|block| !matches!(block, MessageContent::Reasoning { .. }))
        .cloned()
        .collect::<Vec<_>>();
    if content.is_empty() {
        return Ok(None);
    }
    Message::assistant(content)
        .map(|message| Some(Cow::Owned(message)))
        .map_err(|error| ContextError::Invalid(error.to_string()))
}

fn without_unscoped_provider_state(messages: Vec<Message>) -> Result<Vec<Message>> {
    if !messages.iter().any(has_unscoped_provider_state) {
        return Ok(messages);
    }
    messages
        .into_iter()
        .filter_map(|message| without_unscoped_provider_message(Cow::Owned(message)).transpose())
        .map(|message| message.map(Cow::into_owned))
        .collect()
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

fn rejected_tool_message(
    call_id: &str,
    rejection: &rsi_agent_session_protocol::ToolRejection,
) -> Result<Message> {
    Message::tool_result(
        call_id,
        vec![MessageContent::Text {
            text: rejection.message().into_owned(),
        }],
        true,
    )
    .map_err(|error| ContextError::Invalid(error.to_string()))
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

fn input_message(
    source: &InputMessageSource,
    content: &[AgentMessageContent],
    seq: u64,
) -> Result<Message> {
    let content = content
        .iter()
        .enumerate()
        .map(|(index, content)| match content {
            AgentMessageContent::Text { text } => Ok(MessageContent::Text { text: text.clone() }),
            AgentMessageContent::Image { media } => {
                media_descriptor(media).map(MessageContent::Image)
            }
            AgentMessageContent::Reference { reference } => Ok(MessageContent::Text { text: format!(
                "Referenced conversation data from {} through record {}.{}\n{}\nRead more with reference_read using recorded_session_id={}, fact_seq=\"{}\", content_index={}.",
                reference.metadata.source, reference.metadata.through_seq(),
                if reference.metadata.omissions().is_empty() { "" } else { " Earlier material was omitted by capture limits." },
                reference.preview, serde_json::to_string(&reference.metadata.target.session_id).expect("Session identity"), seq, index,
            ) }),
        })
        .collect::<Result<Vec<_>>>()?;
    match source {
        InputMessageSource::AgentInstructions { .. }
        | InputMessageSource::SkillCatalog { .. }
        | InputMessageSource::PluginContext { .. } => {
            let text = content
                .iter()
                .filter_map(|content| match content {
                    MessageContent::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Message::developer_text(text).map_err(|error| ContextError::Invalid(error.to_string()))
        }
        InputMessageSource::Human { .. }
        | InputMessageSource::Continuation { .. }
        | InputMessageSource::Program { .. }
        | InputMessageSource::Agent { .. }
        | InputMessageSource::Completion { .. }
        | InputMessageSource::UserSkillInvocation { .. } => {
            Message::user(content).map_err(|error| ContextError::Invalid(error.to_string()))
        }
    }
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
    encoded_bytes(message)
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
    /// Shared retained and temporary Context credit is unavailable.
    #[error("shared Agent context capacity is exhausted")]
    Capacity,
    /// Fact history or requested limits are invalid.
    #[error("invalid Agent context: {0}")]
    Invalid(String),
    /// Current nonterminal context cannot fit without splitting an active turn.
    #[error("Agent context exceeds its limits after all complete turns were compacted")]
    TooLarge,
}

/// Context result.
pub type Result<T> = std::result::Result<T, ContextError>;

struct CheckpointWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl std::io::Write for CheckpointWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other(
                "encoded checkpoint exceeds its absolute byte bound",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn omission_message(omitted: usize) -> Result<Message> {
    Message::developer_text(format!(
        "[Context omitted {omitted} complete earlier turn(s).]"
    ))
    .map_err(|error| ContextError::Invalid(error.to_string()))
}

#[cfg(test)]
mod checkpoint_encoding_tests {
    use super::*;
    use std::io::Write as _;

    fn accounting_header() -> SessionHeader {
        use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, SessionId};
        SessionHeader::new_local(
            SessionId::new("instruction-accounting").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "test",
                "",
                rsi_ai_protocol::ModelRef::new("test", "test").unwrap(),
                rsi_sandbox::SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn restore_rejects_integrity_bound_invalid_semantic_positions_and_bounds() {
        let header = accounting_header();
        let limits = ContextLimits::default();
        let mut fold =
            ContextFold::with_limits(header.clone(), limits, ContextBudget::default()).unwrap();
        fold.enable_semantic(DefaultContextBuilder::default().identity())
            .unwrap();
        fold.apply(&[SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn").unwrap(),
                text: "task".into(),
                reasoning_effort: None,
                model: None,
                sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                require_approval: false,
            },
        )
        .unwrap()])
            .unwrap();
        let bytes = fold.checkpoint_bytes().unwrap();
        let payload = &bytes[CHECKPOINT_MAGIC.len() + 32..];
        for mutation in 0..4 {
            let mut payload: serde_json::Value = serde_json::from_slice(payload).unwrap();
            match mutation {
                0 => payload["semantic"]["last_human"]["missing-turn"] = serde_json::json!(0),
                1 => {
                    payload["semantic"]["instructions"]["turn"] =
                        serde_json::json!({"10":"SkillCatalog"});
                }
                2 => {
                    let mut source = payload["semantic"]["sources"]["turn"].clone();
                    source["turn"] = serde_json::json!("missing-turn");
                    payload["semantic"]["sources"]["missing-turn"] = source;
                }
                _ => {
                    let humans = payload["semantic"]["last_human"].as_object_mut().unwrap();
                    for index in 0..=MAXIMUM_CONTEXT_MESSAGES {
                        humans.insert(format!("extra-{index}"), serde_json::json!(0));
                    }
                }
            }
            let payload = serde_json::to_vec(&payload).unwrap();
            let mut digest = Sha256::new();
            digest.update(CHECKPOINT_BINDING_DOMAIN);
            digest.update(&payload);
            let mut bytes = CHECKPOINT_MAGIC.to_vec();
            bytes.extend_from_slice(&digest.finalize());
            bytes.extend_from_slice(&payload);
            assert!(
                matches!(
                    ContextFold::from_checkpoint(
                        header.clone(),
                        limits,
                        &bytes,
                        ContextBudget::default()
                    ),
                    Err(ContextError::Invalid(_))
                ),
                "mutation {mutation} was accepted"
            );
        }
    }

    #[test]
    fn instruction_replacement_reaccounts_only_changed_turns() {
        use rsi_agent_session_protocol::{StepId, TurnOutcome};
        let header = accounting_header();
        let mut fold = ContextFold::with_limits(
            header,
            ContextLimits::new(MAXIMUM_CONTEXT_MESSAGES, MAXIMUM_CONTEXT_BYTES).unwrap(),
            ContextBudget::default(),
        )
        .unwrap();
        fold.enable_semantic(DefaultContextBuilder::default().identity())
            .unwrap();
        let instructions = [
            InputMessageSource::AgentInstructions {
                source: "workspace".into(),
                sha256: "a".repeat(64),
                replacement: true,
                tombstone: false,
            },
            InputMessageSource::SkillCatalog {
                sha256: "b".repeat(64),
            },
        ];
        let apply = |fold: &mut ContextFold, body| {
            let seq = fold.through_seq() + 1;
            fold.apply(&[SessionFact::new(seq, seq, body).unwrap()])
                .unwrap();
        };
        let accepted = |turn: &TurnId| SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: "task".into(),
            reasoning_effort: None,
            model: None,
            sandbox: rsi_sandbox::SandboxMode::ReadOnly,
            require_approval: false,
        };
        for index in 0..128 {
            let turn_id = TurnId::new(format!("turn-{index}")).unwrap();
            apply(&mut fold, accepted(&turn_id));
            if index == 0 {
                for source in &instructions {
                    apply(
                        &mut fold,
                        SessionFactBody::InputMessageEntered {
                            turn_id: turn_id.clone(),
                            step_id: StepId::new("old-step").unwrap(),
                            source: source.clone(),
                            content: vec![AgentMessageContent::Text {
                                text: "old instructions".into(),
                            }],
                        },
                    );
                }
            }
            apply(
                &mut fold,
                SessionFactBody::TurnTerminal {
                    turn_id,
                    outcome: TurnOutcome::Completed,
                    result: None,
                },
            );
        }
        let turn_id = TurnId::new("current").unwrap();
        apply(&mut fold, accepted(&turn_id));
        for source in instructions {
            fold.metadata_recounts.set(0);
            apply(
                &mut fold,
                SessionFactBody::InputMessageEntered {
                    turn_id: turn_id.clone(),
                    step_id: StepId::new("new-step").unwrap(),
                    source,
                    content: vec![AgentMessageContent::Text {
                        text: "new instructions".into(),
                    }],
                },
            );
            assert_eq!(
                fold.metadata_recounts.get(),
                2,
                "only the old instruction owner and current Turn need accounting"
            );
            let retained = fold.credit.bytes();
            fold.reaccount().unwrap();
            assert_eq!(
                fold.credit.bytes(),
                retained,
                "incremental accounting equals full accounting"
            );
        }
    }

    #[test]
    fn capped_writer_rejects_before_extending_the_envelope() {
        let mut writer = CheckpointWriter {
            bytes: b"head".to_vec(),
            limit: 8,
        };
        writer.write_all(b"body").unwrap();
        assert!(writer.write_all(b"x").is_err());
        assert_eq!(writer.bytes, b"headbody");
        let mut writer = CheckpointWriter {
            bytes: b"head".to_vec(),
            limit: 8,
        };
        assert!(writer.write_all(&vec![b'x'; 1024]).is_err());
        assert_eq!(writer.bytes, b"head");
    }
}

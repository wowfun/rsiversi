//! Mechanical durable Agent Store seam.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    ActivationId, AgentControlRecord, AgentMessage, AgentMessageSource, ForkTurnSelection,
    InputMessageSource, MAXIMUM_DURABLE_AGENT_TREE_NODES, MAXIMUM_FACTS_PER_READ,
    MAXIMUM_PENDING_AGENT_MESSAGES, MessageDiscardReason, MessageId, MessageTarget, SessionFact,
    SessionFactBody, SessionHeader, SessionId, StepId, TurnId, validate_control_sequence,
    validate_fact_sequence,
};
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Exact `SQLite` and in-memory Store schema version.
pub const AGENT_STORE_SCHEMA_VERSION: u32 = 11;
/// Maximum Facts in one atomic append.
pub const MAXIMUM_STORE_BATCH_FACTS: usize = 512;
/// Maximum encoded bytes in one atomic append.
pub const MAXIMUM_STORE_BATCH_BYTES: usize = 64 * 1024 * 1024;
/// Maximum aggregate encoded bytes returned by one Fact page.
pub const MAXIMUM_STORE_FACT_PAGE_BYTES: usize = MAXIMUM_STORE_BATCH_BYTES;
/// Maximum aggregate encoded bytes returned by one Agent-control page.
pub const MAXIMUM_STORE_CONTROL_PAGE_BYTES: usize = MAXIMUM_STORE_BATCH_BYTES;
/// Maximum aggregate encoded Agent-message bytes returned in one mailbox prefix.
pub const MAXIMUM_STORE_MAILBOX_PAGE_BYTES: usize = 32 * 1024 * 1024;

/// Binds activation labels to the immutable Session Header lineage.
pub fn validate_activation_lineage(
    header: &SessionHeader,
    root: &SessionId,
    parent: Option<&SessionId>,
    path: &rsi_agent_session_protocol::AgentPath,
) -> Result<()> {
    let matches = match header.fork_origin() {
        Some(origin) => {
            root == &origin.root_session_id
                && parent == Some(&origin.parent_session_id)
                && path == &origin.path
        }
        None => root == header.session_id() && parent.is_none() && path.depth() == 0,
    };
    if !matches {
        return Err(StoreError::Invalid(
            "activation lineage disagrees with immutable Header".into(),
        ));
    }
    Ok(())
}

/// Validates that a mailbox claim points to its exact newly appended model-visible input Fact.
pub fn validate_message_claim_fact(
    message: &AgentMessage,
    turn_id: &TurnId,
    step_id: &StepId,
    minimum_entered_fact_seq: u64,
    fact: Option<&SessionFact>,
) -> Result<()> {
    let expected_source = match &message.source {
        AgentMessageSource::Human => InputMessageSource::Human {
            message_id: message.message_id.clone(),
        },
        AgentMessageSource::Agent { source_session_id } => InputMessageSource::Agent {
            message_id: message.message_id.clone(),
            source_session_id: source_session_id.clone(),
        },
        AgentMessageSource::Completion {
            child_session_id,
            activation_id,
        } => InputMessageSource::Completion {
            message_id: message.message_id.clone(),
            child_session_id: child_session_id.clone(),
            activation_id: activation_id.clone(),
        },
    };
    let Some(fact) = fact.filter(|fact| fact.seq() >= minimum_entered_fact_seq) else {
        return Err(StoreError::Invalid(
            "mailbox claim must reference a newly appended input Fact".into(),
        ));
    };
    match fact.body() {
        SessionFactBody::InputMessageEntered {
            turn_id: entered_turn_id,
            step_id: entered_step_id,
            source,
            content,
        } if entered_turn_id == turn_id
            && entered_step_id == step_id
            && source == &expected_source
            && content == &message.content =>
        {
            Ok(())
        }
        _ => Err(StoreError::Invalid(
            "mailbox claim does not match its newly appended input Fact".into(),
        )),
    }
}
/// Maximum immutable CAS object size.
pub const MAXIMUM_STORE_CAS_BYTES: usize = 64 * 1024 * 1024;
/// Maximum durable session identities in one enumeration page.
pub const MAXIMUM_SESSIONS_PER_READ: usize = 256;
/// Maximum opaque Context checkpoint bytes retained for one session.
pub const MAXIMUM_CONTEXT_CHECKPOINT_BYTES: usize =
    rsi_agent_session_protocol::MAXIMUM_CONTEXT_CHECKPOINT_BYTES;

/// Mechanical role of one Fact in the Store's per-turn membership index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreFactTurnRole {
    /// Creates one accepted turn boundary.
    Acceptance,
    /// Closes one accepted turn boundary.
    Terminal,
    /// Belongs strictly inside one currently open turn boundary.
    Event,
}

impl StoreFactTurnRole {
    /// Returns the shared corruption diagnostic for a rejected membership update.
    pub const fn rejected_message(self) -> &'static str {
        match self {
            Self::Acceptance => "durable turn was accepted more than once",
            Self::Terminal => "terminal references a closed or unknown turn",
            Self::Event => "nonterminal Fact references a closed or unknown turn",
        }
    }
}

/// Classifies one validated Fact body for mechanical turn indexing.
pub const fn store_fact_turn_role(body: &SessionFactBody) -> StoreFactTurnRole {
    match body {
        SessionFactBody::TurnAccepted { .. }
        | SessionFactBody::MessageTurnAccepted { .. }
        | SessionFactBody::ImageRequested { .. } => StoreFactTurnRole::Acceptance,
        SessionFactBody::TurnTerminal { .. } => StoreFactTurnRole::Terminal,
        _ => StoreFactTurnRole::Event,
    }
}

/// Opaque bounded Context-owned checkpoint returned by the mechanical Store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredContextCheckpoint {
    /// Immutable Session header fingerprint used for fail-soft invalidation.
    pub header_fingerprint: String,
    /// Exact durable session sequence folded into the checkpoint.
    pub through_seq: u64,
    /// SHA-256 chain of the exact canonical Fact prefix folded by Context.
    pub fact_prefix_sha256: String,
    /// Context-owned versioned bytes.
    pub bytes: Arc<[u8]>,
}

impl StoredContextCheckpoint {
    /// Revalidates mechanical fingerprint, cursor, and byte bounds.
    pub fn validate(&self) -> Result<()> {
        if self.header_fingerprint.len() != 64
            || !self
                .header_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(StoreError::Invalid(
                "checkpoint header fingerprint must be lowercase SHA-256".into(),
            ));
        }
        if self.fact_prefix_sha256.len() != 64
            || !self
                .fact_prefix_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(StoreError::Invalid(
                "checkpoint Fact-prefix digest must be lowercase SHA-256".into(),
            ));
        }
        if self.through_seq == 0
            || self.bytes.is_empty()
            || self.bytes.len() > MAXIMUM_CONTEXT_CHECKPOINT_BYTES
        {
            return Err(StoreError::Invalid(
                "checkpoint cursor and bytes must be nonzero and bounded".into(),
            ));
        }
        Ok(())
    }
}

/// Durable-tail compare-and-set input for one opaque Context checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteContextCheckpoint {
    /// Exact target session.
    pub session_id: SessionId,
    /// Durable tail observed after terminal durability.
    pub expected_durable_seq: u64,
    /// Opaque checkpoint to install.
    pub checkpoint: StoredContextCheckpoint,
}

impl WriteContextCheckpoint {
    /// Revalidates mechanical bounds before Store I/O.
    pub fn validate(&self) -> Result<()> {
        self.checkpoint.validate()?;
        if self.checkpoint.through_seq != self.expected_durable_seq {
            return Err(StoreError::Invalid(
                "checkpoint cursor must equal its expected durable tail".into(),
            ));
        }
        Ok(())
    }
}

/// Atomic compare-and-append input.
#[derive(Clone, Debug)]
pub struct AppendBatch {
    /// Exact target session.
    pub session_id: SessionId,
    /// Current durable sequence observed by the caller.
    pub expected_seq: u64,
    /// Header supplied only when creating the durable session.
    pub header: Option<SessionHeader>,
    /// Nonempty exact contiguous suffix.
    pub facts: Vec<SessionFact>,
}

impl AppendBatch {
    /// Revalidates a complete atomic append without Store state.
    pub fn validate(&self) -> Result<()> {
        if self.facts.is_empty() || self.facts.len() > MAXIMUM_STORE_BATCH_FACTS {
            return Err(StoreError::Invalid(format!(
                "Store append must contain 1..={MAXIMUM_STORE_BATCH_FACTS} Facts"
            )));
        }
        if let Some(header) = &self.header {
            header
                .validate()
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            if header.session_id() != &self.session_id || self.expected_seq != 0 {
                return Err(StoreError::Invalid(
                    "a creation header must match the session and expected sequence zero".into(),
                ));
            }
        }
        validate_fact_sequence(self.expected_seq, &self.facts)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let bytes = self.facts.iter().try_fold(0_usize, |total, fact| {
            total
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Invalid("Store batch size overflow".into()))
        })?;
        if bytes > MAXIMUM_STORE_BATCH_BYTES {
            return Err(StoreError::Invalid(format!(
                "Store append exceeds {MAXIMUM_STORE_BATCH_BYTES} encoded bytes"
            )));
        }
        Ok(())
    }
}

/// Durable watermark returned by one successful append.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppendCommit {
    /// Exact sequence now durable.
    pub durable_seq: u64,
}

/// One session suffix inside a closed multi-session Agent commit.
#[derive(Clone, Debug)]
pub struct AtomicSessionAppend {
    /// Exact target session.
    pub session_id: SessionId,
    /// Current durable Fact sequence observed by the caller.
    pub expected_fact_seq: u64,
    /// Current durable control sequence observed by the caller.
    pub expected_control_seq: u64,
    /// Header supplied only for the single newly durable session.
    pub header: Option<SessionHeader>,
    /// Optional exact contiguous Fact suffix.
    pub facts: Vec<SessionFact>,
    /// Optional exact contiguous Agent-control suffix.
    pub controls: Vec<AgentControlRecord>,
}

impl AtomicSessionAppend {
    fn validate(&self) -> Result<usize> {
        if self.facts.is_empty() && self.controls.is_empty() {
            return Err(StoreError::Invalid(
                "atomic Agent session append must contain Facts or control records".into(),
            ));
        }
        if self.facts.len() > MAXIMUM_STORE_BATCH_FACTS
            || self.controls.len() > MAXIMUM_STORE_BATCH_FACTS
        {
            return Err(StoreError::Invalid(format!(
                "atomic Agent suffixes may contain at most {MAXIMUM_STORE_BATCH_FACTS} records"
            )));
        }
        if let Some(header) = &self.header {
            header
                .validate()
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            if header.session_id() != &self.session_id
                || self.expected_fact_seq != 0
                || self.expected_control_seq != 0
            {
                return Err(StoreError::Invalid(
                    "new Agent session Header must match zero Fact/control cursors".into(),
                ));
            }
        }
        validate_fact_sequence(self.expected_fact_seq, &self.facts)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        validate_control_sequence(self.expected_control_seq, &self.controls)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        self.facts
            .iter()
            .map(SessionFact::encoded_len)
            .chain(self.controls.iter().map(AgentControlRecord::encoded_len))
            .try_fold(0_usize, |total, bytes| {
                total
                    .checked_add(bytes)
                    .ok_or_else(|| StoreError::Invalid("atomic Agent commit size overflow".into()))
            })
    }
}

/// Closed atomic mutation used by Agent-control transitions.
#[derive(Clone, Debug)]
pub struct AtomicAgentCommit {
    /// One source/target pair plus at most one new child session.
    pub sessions: Vec<AtomicSessionAppend>,
    /// Exact active activations which must still own their sessions.
    pub required_active_activations: Vec<AgentActivationGuard>,
    /// Sessions which must have no active activation, open Turn, or waking message.
    pub quiescent_sessions: Vec<SessionId>,
}

impl AtomicAgentCommit {
    /// Revalidates shape, cursor, uniqueness, and aggregate-byte bounds.
    pub fn validate(&self) -> Result<()> {
        if self.sessions.is_empty() || self.sessions.len() > 3 {
            return Err(StoreError::Invalid(
                "atomic Agent commit must touch 1..=3 sessions".into(),
            ));
        }
        if self.required_active_activations.len() > MAXIMUM_SESSIONS_PER_READ
            || self.quiescent_sessions.len() > MAXIMUM_SESSIONS_PER_READ
        {
            return Err(StoreError::Invalid(
                "atomic Agent guard set exceeds its bounded session count".into(),
            ));
        }
        let mut guarded = BTreeSet::new();
        for guard in &self.required_active_activations {
            if !guarded.insert(&guard.session_id) {
                return Err(StoreError::Invalid(
                    "atomic Agent commit repeats an activation guard session".into(),
                ));
            }
        }
        let mut quiescent = BTreeSet::new();
        for session_id in &self.quiescent_sessions {
            if !quiescent.insert(session_id) {
                return Err(StoreError::Invalid(
                    "atomic Agent commit repeats a quiescence guard session".into(),
                ));
            }
        }
        let mut identities = BTreeSet::new();
        let mut headers = 0_usize;
        let mut bytes = 0_usize;
        for session in &self.sessions {
            if !identities.insert(&session.session_id) {
                return Err(StoreError::Invalid(
                    "atomic Agent commit repeats a session".into(),
                ));
            }
            headers += usize::from(session.header.is_some());
            bytes = bytes
                .checked_add(session.validate()?)
                .ok_or_else(|| StoreError::Invalid("atomic Agent commit size overflow".into()))?;
        }
        if headers > 1 {
            return Err(StoreError::Invalid(
                "atomic Agent commit may create at most one session".into(),
            ));
        }
        if bytes > MAXIMUM_STORE_BATCH_BYTES {
            return Err(StoreError::Invalid(format!(
                "atomic Agent commit exceeds {MAXIMUM_STORE_BATCH_BYTES} encoded bytes"
            )));
        }
        Ok(())
    }
}

/// Exact active-activation compare guard for an Agent transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentActivationGuard {
    /// Session which must still own the activation.
    pub session_id: SessionId,
    /// Exact activation identity expected by the caller.
    pub activation_id: ActivationId,
}

/// Indexed lifecycle phase for the single active activation of one session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreActivationPhase {
    /// Its Turn has not durably terminated.
    Running,
    /// Its wait Tool durably released the executor lane.
    Parked,
    /// Its Turn ended but descendant work still prevents settlement.
    WaitingForDescendants,
}

/// Bounded current activation projection maintained from durable controls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreActiveActivation {
    /// Exact active identity.
    pub activation_id: ActivationId,
    /// Optional direct parent session for a non-root activation.
    pub parent_session_id: Option<SessionId>,
    /// Turn claimed by the activation once its waking message is entered.
    pub turn_id: Option<TurnId>,
    /// Current indexed lifecycle phase.
    pub phase: StoreActivationPhase,
    /// Parent-mailbox bytes reserved for this activation's completion.
    pub completion_reserved_bytes: Option<u64>,
}

/// One bounded lexical page of sessions waiting for descendant settlement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreWaitingActivationPage {
    /// Exclusive session cursor supplied by the caller.
    pub after: Option<SessionId>,
    /// Waiting session identities in strict lexical order.
    pub sessions: Vec<SessionId>,
    /// Whether another waiting activation exists.
    pub has_more: bool,
}

impl StoreWaitingActivationPage {
    /// Revalidates bounded strict lexical ordering.
    pub fn validate(&self) -> Result<()> {
        if self.sessions.len() > MAXIMUM_SESSIONS_PER_READ
            || (self.sessions.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "waiting-activation page has invalid bounds".into(),
            ));
        }
        let mut previous = self.after.as_ref();
        for session_id in &self.sessions {
            if previous.is_some_and(|previous| previous >= session_id) {
                return Err(StoreError::Corrupt(
                    "waiting-activation page is not strictly ordered".into(),
                ));
            }
            previous = Some(session_id);
        }
        Ok(())
    }
}

/// Durable Fact/control watermarks for one touched session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentCommitWatermark {
    /// Exact touched session.
    pub session_id: SessionId,
    /// Fact watermark after commit.
    pub durable_fact_seq: u64,
    /// Agent-control watermark after commit.
    pub durable_control_seq: u64,
}

/// Result of one successful atomic Agent commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicAgentCommitResult {
    /// Watermarks in request order.
    pub sessions: Vec<AgentCommitWatermark>,
}

/// One bounded contiguous durable Agent-control page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreControlPage {
    /// Cursor supplied by the caller.
    pub after_seq: u64,
    /// Ordered contiguous records after the cursor.
    pub records: Vec<AgentControlRecord>,
    /// Exact durable control tail at read time.
    pub durable_seq: u64,
}

impl StoreControlPage {
    /// Returns whether this page reaches the read-time durable tail.
    pub fn caught_up(&self) -> bool {
        self.records
            .last()
            .map_or(self.after_seq, AgentControlRecord::seq)
            == self.durable_seq
    }

    /// Revalidates the contiguous bounded page.
    pub fn validate(&self) -> Result<()> {
        validate_control_sequence(self.after_seq, &self.records)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let encoded_bytes = self.records.iter().try_fold(0_usize, |total, record| {
            total
                .checked_add(record.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("control page size overflow".into()))
        })?;
        if encoded_bytes > MAXIMUM_STORE_CONTROL_PAGE_BYTES {
            return Err(StoreError::Corrupt(format!(
                "control page exceeds {MAXIMUM_STORE_CONTROL_PAGE_BYTES} encoded bytes"
            )));
        }
        let last = self
            .records
            .last()
            .map_or(self.after_seq, AgentControlRecord::seq);
        if self.after_seq > self.durable_seq || last > self.durable_seq {
            return Err(StoreError::Corrupt(
                "control page cursor exceeds its durable watermark".into(),
            ));
        }
        Ok(())
    }
}

/// Stable cursor for one root's globally ordered ready messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreReadyMessageCursor {
    /// Timestamp at which the message became waking input.
    pub timestamp_ms: u64,
    /// Target session tie-breaker.
    pub session_id: SessionId,
    /// Per-session control-sequence tie-breaker.
    pub control_seq: u64,
}

/// One unclaimed waking message selected from the durable ready index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreReadyMessage {
    /// Target session.
    pub session_id: SessionId,
    /// Accepted message identity.
    pub message_id: MessageId,
    /// Durable control sequence at which the message became waking input.
    pub control_seq: u64,
    /// Timestamp at which the message became waking input.
    pub timestamp_ms: u64,
    /// Current delivery horizon.
    pub target: MessageTarget,
}

impl StoreReadyMessage {
    /// Returns the exclusive cursor after this entry.
    pub fn cursor(&self) -> StoreReadyMessageCursor {
        StoreReadyMessageCursor {
            timestamp_ms: self.timestamp_ms,
            session_id: self.session_id.clone(),
            control_seq: self.control_seq,
        }
    }
}

/// One bounded page for a single Agent-tree root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreReadyMessagePage {
    /// Exclusive cursor supplied by the caller.
    pub after: Option<StoreReadyMessageCursor>,
    /// Strictly ordered unclaimed waking messages.
    pub messages: Vec<StoreReadyMessage>,
    /// Whether another message exists for this root.
    pub has_more: bool,
}

/// Indexed durable lifecycle of one accepted mailbox message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreAgentMessageState {
    /// Accepted and not yet claimed or discarded.
    Pending,
    /// Atomically entered one activation Turn and Step.
    Claimed {
        /// Owning activation.
        activation_id: ActivationId,
        /// Turn which accepted the message.
        turn_id: TurnId,
        /// Step into which the message entered.
        step_id: StepId,
        /// Exact durable input Fact sequence.
        entered_fact_seq: u64,
    },
    /// Accepted input which will never be claimed.
    Discarded {
        /// Stable discard reason.
        reason: MessageDiscardReason,
        /// Exact control record which discarded it.
        control_seq: u64,
    },
}

/// One bounded mailbox entry projected by the Store-owned message index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreAgentMessage {
    /// Exact accepted message payload.
    pub message: AgentMessage,
    /// Exact compact JSON byte length computed at the Store-owned payload boundary.
    pub encoded_message_bytes: usize,
    /// Root which owns the target Agent tree.
    pub root_session_id: SessionId,
    /// Current delivery horizon.
    pub target: MessageTarget,
    /// Whether pending input belongs in the waking ready index.
    pub wake_required: bool,
    /// Exact acceptance control sequence.
    pub accepted_control_seq: u64,
    /// Current indexed lifecycle.
    pub state: StoreAgentMessageState,
}

impl StoreAgentMessage {
    /// Revalidates one indexed projection independently of its table encoding.
    pub fn validate(&self, durable_control_seq: u64) -> Result<()> {
        self.message
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        if self.encoded_message_bytes == 0 {
            return Err(StoreError::Corrupt(
                "mailbox message has no encoded-byte length".into(),
            ));
        }
        if self.accepted_control_seq == 0 || self.accepted_control_seq > durable_control_seq {
            return Err(StoreError::Corrupt(
                "mailbox acceptance exceeds the durable control watermark".into(),
            ));
        }
        match &self.state {
            StoreAgentMessageState::Claimed {
                entered_fact_seq, ..
            } if *entered_fact_seq == 0 => {
                return Err(StoreError::Corrupt(
                    "claimed mailbox message has no entered Fact".into(),
                ));
            }
            StoreAgentMessageState::Discarded { control_seq, .. }
                if *control_seq <= self.accepted_control_seq
                    || *control_seq > durable_control_seq =>
            {
                return Err(StoreError::Corrupt(
                    "mailbox discard sequence is outside its durable interval".into(),
                ));
            }
            StoreAgentMessageState::Pending
            | StoreAgentMessageState::Claimed { .. }
            | StoreAgentMessageState::Discarded { .. } => {}
        }
        Ok(())
    }
}

/// Atomic bounded view of one session mailbox at one control watermark.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreAgentMailbox {
    /// Optional exact message requested by identity, including terminal states.
    pub selected: Option<StoreAgentMessage>,
    /// Every pending message in acceptance order.
    pub pending: Vec<StoreAgentMessage>,
    /// Complete pending count at the same Store snapshot, including omitted suffix entries.
    pub pending_count: usize,
    /// Exact durable control tail at read time.
    pub durable_control_seq: u64,
    /// Exact durable Fact tail captured in the same Store snapshot.
    pub durable_fact_seq: u64,
}

/// Atomic metadata-only view of one session mailbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreAgentMailboxSummary {
    /// Complete pending-message count at the Store snapshot.
    pub pending_count: usize,
    /// Pending next-Step completion identities in acceptance order at the same snapshot.
    pub pending_next_step_completion_message_ids: Vec<MessageId>,
    /// Exact durable control tail at the same Store snapshot.
    pub durable_control_seq: u64,
    /// Exact durable Fact tail at the same Store snapshot.
    pub durable_fact_seq: u64,
}

/// Latest workspace-context digests derived from canonical durable Facts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoreWorkspaceContextState {
    /// Latest complete instruction-baseline digest, when one was published.
    pub instructions_sha256: Option<String>,
    /// Latest complete skill-catalog digest, when one was published.
    pub skill_catalog_sha256: Option<String>,
    /// Exact durable Fact tail captured in the same Store snapshot.
    pub durable_fact_seq: u64,
}

impl StoreWorkspaceContextState {
    /// Revalidates optional lowercase SHA-256 values returned by a Store.
    pub fn validate(&self) -> Result<()> {
        for (name, digest) in [
            ("workspace instruction", &self.instructions_sha256),
            ("workspace skill catalog", &self.skill_catalog_sha256),
        ] {
            if digest.as_ref().is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            }) {
                return Err(StoreError::Corrupt(format!(
                    "{name} digest is not lowercase SHA-256"
                )));
            }
        }
        Ok(())
    }
}

impl StoreAgentMailboxSummary {
    /// Revalidates the protocol-owned pending-message bound.
    pub fn validate(&self) -> Result<()> {
        let identities = self
            .pending_next_step_completion_message_ids
            .iter()
            .collect::<BTreeSet<_>>();
        if self.pending_count > MAXIMUM_PENDING_AGENT_MESSAGES
            || self.pending_next_step_completion_message_ids.len() > self.pending_count
            || identities.len() != self.pending_next_step_completion_message_ids.len()
        {
            return Err(StoreError::Corrupt(
                "mailbox summary exceeds its bound or repeats a next-Step identity".into(),
            ));
        }
        Ok(())
    }
}

impl StoreAgentMailbox {
    /// Revalidates bounds, ordering, identities, and indexed state.
    pub fn validate(&self) -> Result<()> {
        if self.pending_count > MAXIMUM_PENDING_AGENT_MESSAGES
            || self.pending.len() > self.pending_count
            || (self.pending_count > 0 && self.pending.is_empty())
        {
            return Err(StoreError::Corrupt(
                "indexed mailbox exceeds its pending-message bound".into(),
            ));
        }
        let mut previous = 0_u64;
        let mut identities = BTreeSet::new();
        let mut encoded_bytes = 0_usize;
        for entry in &self.pending {
            entry.validate(self.durable_control_seq)?;
            encoded_bytes = encoded_bytes
                .checked_add(entry.encoded_message_bytes)
                .ok_or_else(|| StoreError::Corrupt("mailbox byte count overflowed".into()))?;
            if !matches!(entry.state, StoreAgentMessageState::Pending)
                || entry.accepted_control_seq <= previous
                || !identities.insert(&entry.message.message_id)
            {
                return Err(StoreError::Corrupt(
                    "indexed pending mailbox is not strictly ordered and unique".into(),
                ));
            }
            previous = entry.accepted_control_seq;
        }
        if encoded_bytes > MAXIMUM_STORE_MAILBOX_PAGE_BYTES {
            return Err(StoreError::Corrupt(
                "indexed mailbox page exceeds its encoded-byte bound".into(),
            ));
        }
        if let Some(selected) = &self.selected {
            selected.validate(self.durable_control_seq)?;
        }
        Ok(())
    }
}

/// One bounded lexical page of Agent-tree roots with waking messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreReadyRootPage {
    /// Exclusive root cursor supplied by the caller.
    pub after: Option<SessionId>,
    /// Distinct roots with at least one waking message.
    pub roots: Vec<SessionId>,
    /// Whether another ready root exists.
    pub has_more: bool,
}

impl StoreReadyRootPage {
    /// Revalidates bounded strict lexical ordering.
    pub fn validate(&self) -> Result<()> {
        if self.roots.len() > MAXIMUM_SESSIONS_PER_READ || (self.roots.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "ready-root page has invalid bounds".into(),
            ));
        }
        let mut previous = self.after.as_ref();
        for root in &self.roots {
            if previous.is_some_and(|previous| previous >= root) {
                return Err(StoreError::Corrupt(
                    "ready-root page is not strictly ordered".into(),
                ));
            }
            previous = Some(root);
        }
        Ok(())
    }
}

/// One durable direct-child descriptor from immutable Header lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreAgentChild {
    /// Exact child session identity.
    pub session_id: SessionId,
    /// Stable path within the shared Agent tree.
    pub path: rsi_agent_session_protocol::AgentPath,
    /// Stable parent-allocated task name.
    pub task_name: String,
}

/// One bounded lexical page of direct Agent children.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreAgentChildPage {
    /// Exclusive child-session cursor supplied by the caller.
    pub after: Option<SessionId>,
    /// Direct children ordered by session identity.
    pub children: Vec<StoreAgentChild>,
    /// Whether another direct child exists.
    pub has_more: bool,
}

impl StoreAgentChildPage {
    /// Revalidates page bounds, direct-child paths, and lexical ordering.
    pub fn validate(&self) -> Result<()> {
        if self.children.len() > MAXIMUM_SESSIONS_PER_READ
            || (self.children.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "Agent-child page has invalid bounds".into(),
            ));
        }
        let mut previous = self.after.as_ref();
        for child in &self.children {
            if previous.is_some_and(|previous| previous >= &child.session_id)
                || child.path.depth() == 0
            {
                return Err(StoreError::Corrupt(
                    "Agent-child page is not strictly ordered or names a root".into(),
                ));
            }
            previous = Some(&child.session_id);
        }
        Ok(())
    }
}

/// One descendant's durable Agent-control watermark captured in a subtree snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreDescendantControlWatermark {
    /// Exact descendant Session identity.
    pub session_id: SessionId,
    /// Exact durable Agent-control tail captured with subtree membership.
    pub durable_control_seq: u64,
}

/// Atomic bounded view of one Session's complete durable descendant set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreDescendantControlSnapshot {
    /// Descendants in strict lexical Session order.
    pub descendants: Vec<StoreDescendantControlWatermark>,
}

impl StoreDescendantControlSnapshot {
    /// Revalidates the tree-size bound and strict lexical identity ordering.
    pub fn validate(&self) -> Result<()> {
        if self.descendants.len() >= MAXIMUM_DURABLE_AGENT_TREE_NODES {
            return Err(StoreError::Corrupt(
                "descendant control snapshot exceeds the durable tree bound".into(),
            ));
        }
        let mut previous = None;
        for descendant in &self.descendants {
            if previous.is_some_and(|previous| previous >= &descendant.session_id) {
                return Err(StoreError::Corrupt(
                    "descendant control snapshot is not strictly ordered".into(),
                ));
            }
            previous = Some(&descendant.session_id);
        }
        Ok(())
    }
}

impl StoreReadyMessagePage {
    /// Revalidates ordering, count, and cursor semantics.
    pub fn validate(&self) -> Result<()> {
        if self.messages.len() > MAXIMUM_SESSIONS_PER_READ
            || (self.messages.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "ready-message page has invalid bounds".into(),
            ));
        }
        let mut previous = self.after.clone();
        for message in &self.messages {
            let current = message.cursor();
            if previous.as_ref().is_some_and(|previous| {
                (
                    current.timestamp_ms,
                    &current.session_id,
                    current.control_seq,
                ) <= (
                    previous.timestamp_ms,
                    &previous.session_id,
                    previous.control_seq,
                )
            }) {
                return Err(StoreError::Corrupt(
                    "ready-message page is not strictly ordered".into(),
                ));
            }
            previous = Some(current);
        }
        Ok(())
    }
}

/// One bounded contiguous durable Fact page.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreFactPage {
    /// Cursor supplied by the caller.
    pub after_seq: u64,
    /// Ordered contiguous Facts after the cursor.
    pub facts: Vec<SessionFact>,
    /// Exact durable tail at read time.
    pub durable_seq: u64,
}

impl StoreFactPage {
    /// Returns whether this page reaches the read-time durable tail.
    pub fn caught_up(&self) -> bool {
        self.facts.last().map_or(self.after_seq, SessionFact::seq) == self.durable_seq
    }

    /// Revalidates cursor, contiguity, and watermark relationships.
    pub fn validate(&self) -> Result<()> {
        validate_fact_sequence(self.after_seq, &self.facts)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let last = self.facts.last().map_or(self.after_seq, SessionFact::seq);
        if self.after_seq > self.durable_seq || last > self.durable_seq {
            return Err(StoreError::Corrupt(
                "Fact page cursor exceeds its durable watermark".into(),
            ));
        }
        let bytes = self.facts.iter().try_fold(0_usize, |total, fact| {
            total
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("Fact page size overflow".into()))
        })?;
        if bytes > MAXIMUM_STORE_FACT_PAGE_BYTES {
            return Err(StoreError::Corrupt(format!(
                "Fact page exceeds {MAXIMUM_STORE_FACT_PAGE_BYTES} encoded bytes"
            )));
        }
        Ok(())
    }
}

/// One bounded backward page returned in ascending durable sequence order.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreBackwardFactPage {
    /// Effective exclusive cursor. Input zero resolves to one past the read-time tail.
    pub before_seq: u64,
    /// Ordered contiguous Facts immediately before the cursor.
    pub facts: Vec<SessionFact>,
    /// Exact durable tail at read time.
    pub durable_seq: u64,
    /// Whether at least one earlier Fact existed at read time.
    pub has_more: bool,
}

impl StoreBackwardFactPage {
    /// Revalidates cursor, contiguity, watermark, count, and byte bounds.
    pub fn validate(&self) -> Result<()> {
        let maximum_before = self
            .durable_seq
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("durable sequence is exhausted".into()))?;
        if self.before_seq == 0
            || self.before_seq > maximum_before
            || self.facts.len() > MAXIMUM_FACTS_PER_READ
            || (self.facts.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "backward Fact page has invalid cursor or count bounds".into(),
            ));
        }
        let expected_after = self
            .facts
            .first()
            .map_or(self.before_seq - 1, |fact| fact.seq() - 1);
        validate_fact_sequence(expected_after, &self.facts)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        match self.facts.last() {
            Some(fact) if fact.seq() != self.before_seq - 1 => {
                return Err(StoreError::Corrupt(
                    "backward Fact page does not end immediately before its cursor".into(),
                ));
            }
            None if self.before_seq != 1 => {
                return Err(StoreError::Corrupt(
                    "empty backward Fact page does not begin at the initial cursor".into(),
                ));
            }
            _ => {}
        }
        let bytes = self.facts.iter().try_fold(0_usize, |total, fact| {
            total
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("backward Fact page size overflow".into()))
        })?;
        if bytes > MAXIMUM_STORE_FACT_PAGE_BYTES {
            return Err(StoreError::Corrupt(format!(
                "backward Fact page exceeds {MAXIMUM_STORE_FACT_PAGE_BYTES} encoded bytes"
            )));
        }
        Ok(())
    }
}

/// One bounded ordered page selected from a single durable turn stream.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreTurnFactPage {
    /// Exact selected turn.
    pub turn_id: TurnId,
    /// Exclusive session-sequence cursor supplied by the caller.
    pub after_seq: u64,
    /// Ordered Facts belonging to the selected turn after the cursor.
    pub facts: Vec<SessionFact>,
    /// Exact session durable tail at read time.
    pub durable_seq: u64,
    /// Whether at least one later Fact for this turn existed at read time.
    pub has_more: bool,
}

impl StoreTurnFactPage {
    /// Revalidates selection, ordering, watermark, and page bounds.
    pub fn validate(&self) -> Result<()> {
        if self.after_seq > self.durable_seq
            || self.facts.len() > MAXIMUM_FACTS_PER_READ
            || (self.facts.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "turn Fact page has invalid cursor or count bounds".into(),
            ));
        }
        let mut previous = self.after_seq;
        let mut encoded_bytes = 0_usize;
        for fact in &self.facts {
            if fact.seq() <= previous || fact.seq() > self.durable_seq {
                return Err(StoreError::Corrupt(
                    "turn Fact page is not strictly ordered within its watermark".into(),
                ));
            }
            if fact.body().turn_id() != &self.turn_id {
                return Err(StoreError::Corrupt(
                    "turn Fact page contains a Fact for another turn".into(),
                ));
            }
            encoded_bytes = encoded_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("turn Fact page size overflow".into()))?;
            previous = fact.seq();
        }
        if encoded_bytes > MAXIMUM_STORE_FACT_PAGE_BYTES {
            return Err(StoreError::Corrupt(format!(
                "turn Fact page exceeds {MAXIMUM_STORE_FACT_PAGE_BYTES} encoded bytes"
            )));
        }
        Ok(())
    }
}

/// Mechanically validated acceptance and optional terminal boundary for one turn.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreTurnBoundary {
    turn_id: TurnId,
    accepted: SessionFact,
    terminal: Option<SessionFact>,
    durable_seq: u64,
}

/// Immutable completed-parent boundary resolved for one fork request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreForkBoundary {
    /// Exclusive Fact cursor before the first selected completed Turn.
    pub resolved_after_seq: u64,
    /// Latest inherited terminal sequence, or zero for no inherited turns.
    pub resolved_terminal_seq: u64,
    /// Fact-prefix digest at `resolved_terminal_seq`.
    pub terminal_prefix_sha256: String,
    /// Completed turns selected by the request.
    pub effective_turns: u64,
}

impl StoreTurnBoundary {
    /// Validates indexed boundary Facts and constructs one narrow Store result.
    pub fn new(
        turn_id: TurnId,
        accepted: SessionFact,
        terminal: Option<SessionFact>,
        durable_seq: u64,
    ) -> Result<Self> {
        accepted
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        if accepted.seq() > durable_seq
            || accepted.body().turn_id() != &turn_id
            || !matches!(
                accepted.body(),
                SessionFactBody::TurnAccepted { .. }
                    | SessionFactBody::MessageTurnAccepted { .. }
                    | SessionFactBody::ImageRequested { .. }
            )
        {
            return Err(StoreError::Corrupt(
                "turn boundary acceptance does not match its index".into(),
            ));
        }
        if let Some(terminal) = &terminal {
            terminal
                .validate()
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            if terminal.seq() <= accepted.seq()
                || terminal.seq() > durable_seq
                || terminal.body().turn_id() != &turn_id
                || !matches!(terminal.body(), SessionFactBody::TurnTerminal { .. })
            {
                return Err(StoreError::Corrupt(
                    "turn boundary terminal does not match its index".into(),
                ));
            }
        }
        Ok(Self {
            turn_id,
            accepted,
            terminal,
            durable_seq,
        })
    }

    /// Returns the exact selected turn.
    pub const fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Returns the session sequence containing turn acceptance.
    pub const fn accepted_seq(&self) -> u64 {
        self.accepted.seq()
    }

    /// Returns the exact typed acceptance Fact.
    pub const fn accepted(&self) -> &SessionFact {
        &self.accepted
    }

    /// Returns the optional typed terminal Fact.
    pub const fn terminal(&self) -> Option<&SessionFact> {
        self.terminal.as_ref()
    }

    /// Returns the exact session durable tail observed with the boundary.
    pub const fn durable_seq(&self) -> u64 {
        self.durable_seq
    }

    /// Consumes the boundary into its validated indexed values.
    pub fn into_parts(self) -> (TurnId, SessionFact, Option<SessionFact>, u64) {
        (self.turn_id, self.accepted, self.terminal, self.durable_seq)
    }
}

/// One mechanically indexed accepted turn without a terminal Fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreOpenTurn {
    /// Exact durable turn identity.
    pub turn_id: TurnId,
    /// Session Fact sequence containing its acceptance.
    pub accepted_seq: u64,
}

/// One bounded page of open turns ordered by acceptance sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreOpenTurnPage {
    /// Exclusive acceptance-sequence cursor supplied by the caller.
    pub after_accepted_seq: u64,
    /// Open turns after the cursor.
    pub turns: Vec<StoreOpenTurn>,
    /// Exact session durable tail at read time.
    pub durable_seq: u64,
    /// Whether at least one later open turn existed at read time.
    pub has_more: bool,
}

impl StoreOpenTurnPage {
    /// Revalidates ordering, uniqueness, cursor, and page bounds.
    pub fn validate(&self) -> Result<()> {
        if self.after_accepted_seq > self.durable_seq
            || self.turns.len() > MAXIMUM_FACTS_PER_READ
            || (self.turns.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "open-turn page has invalid cursor or count bounds".into(),
            ));
        }
        let mut previous = self.after_accepted_seq;
        let mut identities = BTreeSet::new();
        for turn in &self.turns {
            if turn.accepted_seq <= previous || turn.accepted_seq > self.durable_seq {
                return Err(StoreError::Corrupt(
                    "open-turn page is not strictly ordered within its watermark".into(),
                ));
            }
            if !identities.insert(&turn.turn_id) {
                return Err(StoreError::Corrupt(
                    "open-turn page repeats a turn identity".into(),
                ));
            }
            previous = turn.accepted_seq;
        }
        Ok(())
    }
}

/// One bounded lexical page of durable session identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreSessionPage {
    /// Exclusive lexical cursor supplied by the caller.
    pub after: Option<SessionId>,
    /// Strictly increasing identities after the cursor.
    pub sessions: Vec<SessionId>,
    /// Whether at least one later identity existed at read time.
    pub has_more: bool,
}

impl StoreSessionPage {
    /// Revalidates ordering, cursor, and page bounds.
    pub fn validate(&self) -> Result<()> {
        if self.sessions.len() > MAXIMUM_SESSIONS_PER_READ
            || (self.sessions.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "session enumeration page has invalid bounds".into(),
            ));
        }
        let mut previous = self.after.as_ref();
        for session in &self.sessions {
            if previous.is_some_and(|previous| session <= previous) {
                return Err(StoreError::Corrupt(
                    "session enumeration is not strictly lexical".into(),
                ));
            }
            previous = Some(session);
        }
        Ok(())
    }
}

/// Exact cursor for creation-time-descending session enumeration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreRecentSessionCursor {
    /// Durable creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Session identity used as the deterministic tie-breaker.
    pub session_id: SessionId,
}

/// One durable session summary ordered by creation time and identity descending.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreRecentSession {
    /// Complete validated immutable Header selected in the listing snapshot.
    pub header: SessionHeader,
}

impl StoreRecentSession {
    /// Returns the exact cursor selecting rows after this summary.
    pub fn cursor(&self) -> StoreRecentSessionCursor {
        StoreRecentSessionCursor {
            created_at_ms: self.header.created_at_ms(),
            session_id: self.header.session_id().clone(),
        }
    }
}

/// One bounded creation-time-descending page of durable sessions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreRecentSessionPage {
    /// Exclusive descending cursor supplied by the caller.
    pub after: Option<StoreRecentSessionCursor>,
    /// Strictly descending rows after the cursor.
    pub sessions: Vec<StoreRecentSession>,
    /// Whether at least one later page row existed at read time.
    pub has_more: bool,
}

impl StoreRecentSessionPage {
    /// Revalidates ordering, cursor, timestamp, and page bounds.
    pub fn validate(&self) -> Result<()> {
        if self.sessions.len() > MAXIMUM_SESSIONS_PER_READ
            || (self.sessions.is_empty() && self.has_more)
        {
            return Err(StoreError::Corrupt(
                "recent-session page has invalid bounds".into(),
            ));
        }
        let mut previous = self.after.clone();
        for session in &self.sessions {
            let current = session.cursor();
            if previous.as_ref().is_some_and(|previous| {
                (current.created_at_ms, &current.session_id)
                    >= (previous.created_at_ms, &previous.session_id)
            }) {
                return Err(StoreError::Corrupt(
                    "recent-session page is not strictly descending".into(),
                ));
            }
            previous = Some(current);
        }
        Ok(())
    }
}

/// Immutable CAS object identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CasObjectRef {
    /// Lowercase SHA-256 digest.
    pub sha256: String,
    /// Exact object byte length.
    pub byte_len: u64,
}

impl<'de> Deserialize<'de> for CasObjectRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRef {
            sha256: String,
            byte_len: u64,
        }
        let wire = WireRef::deserialize(deserializer)?;
        let reference = Self {
            sha256: wire.sha256,
            byte_len: wire.byte_len,
        };
        reference
            .validate()
            .map(|()| reference)
            .map_err(serde::de::Error::custom)
    }
}

impl CasObjectRef {
    /// Revalidates a bounded immutable object reference.
    pub fn validate(&self) -> Result<()> {
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || self.byte_len == 0
            || self.byte_len > MAXIMUM_STORE_CAS_BYTES as u64
        {
            return Err(StoreError::Invalid(
                "CAS reference must contain lowercase SHA-256 and bounded nonzero length".into(),
            ));
        }
        Ok(())
    }
}

/// Mechanical durable operations under one already-held writer lease.
#[async_trait]
pub trait SessionStore: fmt::Debug + Send + Sync + 'static {
    /// Atomically creates a session if needed and appends one exact suffix.
    async fn append(&self, batch: AppendBatch) -> Result<AppendCommit>;
    /// Applies one closed Agent-control commit across up to three sessions.
    async fn commit_agent(&self, commit: AtomicAgentCommit) -> Result<AtomicAgentCommitResult> {
        commit.validate()?;
        Err(StoreError::Invalid(
            "this Agent Store does not support Agent-control commits".into(),
        ))
    }
    /// Reads the immutable header or returns not found.
    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader>;
    /// Reads at most `limit` contiguous Facts after one cursor.
    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage>;
    /// Reads at most `limit` contiguous Agent-control records after one cursor.
    async fn read_controls(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreControlPage> {
        let _ = (session_id, after_seq);
        validate_read_limit(limit)?;
        Err(StoreError::Invalid(
            "this Agent Store does not support Agent-control reads".into(),
        ))
    }
    /// Reads at most `limit` contiguous Facts immediately before one exclusive cursor.
    ///
    /// Cursor zero selects one past the read-time durable tail. Results are
    /// always returned in ascending sequence order.
    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> Result<StoreBackwardFactPage>;
    /// Reads at most `limit` ordered Facts for one exact turn after a session cursor.
    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreTurnFactPage>;
    /// Reads and validates the indexed acceptance and optional terminal boundary for one turn.
    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> Result<StoreTurnBoundary>;
    /// Resolves the tamper-evident completed-turn prefix before one invoking Turn.
    async fn resolve_fork_boundary(
        &self,
        session_id: &SessionId,
        invoking_turn_id: &TurnId,
        selection: ForkTurnSelection,
    ) -> Result<StoreForkBoundary> {
        let _ = (session_id, invoking_turn_id, selection);
        Err(StoreError::Invalid(
            "this Agent Store does not support fork boundaries".into(),
        ))
    }
    /// Lists at most `limit` accepted turns without a terminal Fact.
    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> Result<StoreOpenTurnPage>;
    /// Lists at most `limit` durable session identities after an exclusive lexical cursor.
    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage>;
    /// Lists at most `limit` sessions after an exclusive creation-time-descending cursor.
    async fn list_recent_sessions(
        &self,
        after: Option<&StoreRecentSessionCursor>,
        limit: usize,
    ) -> Result<StoreRecentSessionPage>;
    /// Lists at most `limit` sessions containing at least one open turn after
    /// an exclusive lexical cursor.
    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage>;
    /// Lists unclaimed waking messages for one exact Agent-tree root.
    async fn list_ready_messages(
        &self,
        root_session_id: &SessionId,
        after: Option<&StoreReadyMessageCursor>,
        limit: usize,
    ) -> Result<StoreReadyMessagePage> {
        let _ = (root_session_id, after);
        validate_session_read_limit(limit)?;
        Err(StoreError::Invalid(
            "this Agent Store does not support durable ready messages".into(),
        ))
    }
    /// Reads the complete bounded pending mailbox and one optional message status atomically.
    async fn read_agent_mailbox(
        &self,
        session_id: &SessionId,
        selected_message_id: Option<&MessageId>,
    ) -> Result<StoreAgentMailbox> {
        let _ = (session_id, selected_message_id);
        Err(StoreError::Invalid(
            "this Agent Store does not support indexed mailbox reads".into(),
        ))
    }
    /// Reads only the complete pending count and exact durable tails atomically.
    async fn read_agent_mailbox_summary(
        &self,
        session_id: &SessionId,
    ) -> Result<StoreAgentMailboxSummary> {
        let _ = session_id;
        Err(StoreError::Invalid(
            "this Agent Store does not support mailbox summaries".into(),
        ))
    }
    /// Reads the latest workspace-context digests derived from canonical Facts.
    async fn read_workspace_context_state(
        &self,
        session_id: &SessionId,
    ) -> Result<StoreWorkspaceContextState> {
        let _ = session_id;
        Err(StoreError::Invalid(
            "this Agent Store does not support workspace-context state".into(),
        ))
    }
    /// Lists distinct Agent-tree roots which currently contain waking input.
    async fn list_ready_roots(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreReadyRootPage> {
        let _ = after;
        validate_session_read_limit(limit)?;
        Err(StoreError::Invalid(
            "this Agent Store does not support durable ready-root listing".into(),
        ))
    }
    /// Lists bounded direct children recorded by immutable fork lineage.
    async fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreAgentChildPage> {
        let _ = (parent_session_id, after);
        validate_session_read_limit(limit)?;
        Err(StoreError::Invalid(
            "this Agent Store does not support durable child listing".into(),
        ))
    }
    /// Atomically snapshots complete descendant membership and control watermarks.
    async fn read_descendant_control_snapshot(
        &self,
        parent_session_id: &SessionId,
    ) -> Result<StoreDescendantControlSnapshot> {
        let _ = parent_session_id;
        Err(StoreError::Invalid(
            "this Agent Store does not support descendant control snapshots".into(),
        ))
    }
    /// Reads the single currently active activation projection for one session.
    async fn active_activation(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoreActiveActivation>> {
        let _ = session_id;
        Err(StoreError::Invalid(
            "this Agent Store does not support active-activation indexing".into(),
        ))
    }
    /// Counts active child activations reserving completion capacity in a parent mailbox.
    async fn completion_reservation_count(&self, parent_session_id: &SessionId) -> Result<usize> {
        let _ = parent_session_id;
        Err(StoreError::Invalid(
            "this Agent Store does not support completion-reservation indexing".into(),
        ))
    }
    /// Lists sessions whose terminal Turn still waits for descendants.
    async fn list_waiting_activations(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreWaitingActivationPage> {
        let _ = after;
        validate_session_read_limit(limit)?;
        Err(StoreError::Invalid(
            "this Agent Store does not support waiting-activation listing".into(),
        ))
    }
    /// Reads one optional opaque Context checkpoint.
    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoredContextCheckpoint>> {
        let _ = session_id;
        Ok(None)
    }
    /// Installs a checkpoint only if the durable tail still matches exactly.
    async fn write_context_checkpoint(&self, write: WriteContextCheckpoint) -> Result<()> {
        write.validate()?;
        Err(StoreError::Invalid(
            "this Agent Store does not support Context checkpoints".into(),
        ))
    }
    /// Publishes bounded immutable bytes and returns their computed identity.
    async fn put_cas(&self, bytes: Arc<[u8]>) -> Result<CasObjectRef>;
    /// Reads and verifies one immutable object.
    async fn read_cas(&self, object: &CasObjectRef) -> Result<Arc<[u8]>>;
}

/// Nominal process-local Store contract.
#[derive(Debug)]
pub struct SessionStoreContract;

impl LocalContract for SessionStoreContract {
    const KEY: &'static str = "rsi.agent.store";
    type Service = dyn SessionStore;
}

/// Closed Store failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StoreError {
    /// Malformed or out-of-bounds input.
    #[error("invalid Agent Store input: {0}")]
    Invalid(String),
    /// Session or CAS object is absent.
    #[error("Agent Store object was not found: {0}")]
    NotFound(String),
    /// Turn is absent from an existing session.
    #[error("Agent Store turn `{turn}` was not found in session `{session}`")]
    TurnNotFound {
        /// Exact durable session identity.
        session: String,
        /// Exact durable turn identity.
        turn: String,
    },
    /// Compare-and-append observed a different durable tail.
    #[error("Agent Store conflict: expected sequence {expected}, actual {actual}")]
    Conflict {
        /// Caller-observed sequence.
        expected: u64,
        /// Current durable sequence.
        actual: u64,
    },
    /// Agent-control compare-and-append observed a different durable tail.
    #[error(
        "Agent Store control conflict for session `{session}`: expected sequence {expected}, actual {actual}"
    )]
    ControlConflict {
        /// Exact target session.
        session: String,
        /// Caller-observed control sequence.
        expected: u64,
        /// Current durable control sequence.
        actual: u64,
    },
    /// A required activation no longer owns its session.
    #[error("Agent Store activation guard failed for session `{session}`")]
    ActivationGuardConflict {
        /// Exact guarded session.
        session: String,
    },
    /// A guarded session still has work that prevents settlement.
    #[error("Agent Store session `{session}` is not quiescent")]
    SessionNotQuiescent {
        /// Exact guarded session.
        session: String,
    },
    /// On-disk schema is not the exact supported schema.
    #[error("Agent Store schema mismatch: expected {expected}, actual {actual}")]
    SchemaMismatch {
        /// Exact implementation schema.
        expected: u32,
        /// Observed schema, including zero for an invalid partial layout.
        actual: u32,
    },
    /// Another process holds the Store-root writer lease.
    #[error("Agent Store root already has an active writer")]
    WriterLocked,
    /// Durable bytes violate protocol invariants.
    #[error("Agent Store is corrupt: {0}")]
    Corrupt(String),
    /// Bounded storage I/O failure.
    #[error("Agent Store I/O failed: {0}")]
    Io(String),
}

/// Store result.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Validates one Store read limit.
pub fn validate_read_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
        return Err(StoreError::Invalid(format!(
            "Fact read limit must be within 1..={MAXIMUM_FACTS_PER_READ}"
        )));
    }
    Ok(())
}

/// Validates one session enumeration limit.
pub fn validate_session_read_limit(limit: usize) -> Result<()> {
    if limit == 0 || limit > MAXIMUM_SESSIONS_PER_READ {
        return Err(StoreError::Invalid(format!(
            "session read limit must be within 1..={MAXIMUM_SESSIONS_PER_READ}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentControlRecordBody, AgentMessageContent, AgentMessageSource, MessageOptions,
    };

    fn cancellation_fact(sequence: u64) -> SessionFact {
        SessionFact::new(
            sequence,
            sequence,
            SessionFactBody::CancelRequested {
                turn_id: TurnId::new("turn-backward-page").unwrap(),
                reason: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn append_batch_rejects_an_empty_fact_suffix() {
        let batch = AppendBatch {
            session_id: SessionId::new("session-empty-append").unwrap(),
            expected_seq: 0,
            header: None,
            facts: Vec::new(),
        };

        assert_eq!(
            batch.validate(),
            Err(StoreError::Invalid(format!(
                "Store append must contain 1..={MAXIMUM_STORE_BATCH_FACTS} Facts"
            )))
        );
    }

    #[test]
    fn backward_page_must_end_immediately_before_its_cursor() {
        let skipped_suffix = StoreBackwardFactPage {
            before_seq: 8,
            facts: vec![cancellation_fact(4), cancellation_fact(5)],
            durable_seq: 8,
            has_more: true,
        };
        assert!(matches!(
            skipped_suffix.validate(),
            Err(StoreError::Corrupt(message)) if message.contains("cursor")
        ));

        let impossible_empty_page = StoreBackwardFactPage {
            before_seq: 8,
            facts: Vec::new(),
            durable_seq: 8,
            has_more: false,
        };
        assert!(matches!(
            impossible_empty_page.validate(),
            Err(StoreError::Corrupt(message)) if message.contains("cursor")
        ));
    }

    #[test]
    fn control_page_rejects_an_aggregate_larger_than_the_read_budget() {
        let records = (1_u64..=65)
            .map(|sequence| {
                AgentControlRecord::new(
                    sequence,
                    sequence,
                    AgentControlRecordBody::MessageAccepted {
                        message: AgentMessage {
                            message_id: MessageId::new(format!("large-control-{sequence}"))
                                .unwrap(),
                            source: AgentMessageSource::Human,
                            content: vec![AgentMessageContent::Text {
                                text: "x"
                                    .repeat(rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES),
                            }],
                            options: MessageOptions::default(),
                        },
                        root_session_id: SessionId::new("large-control-root").unwrap(),
                        target: MessageTarget::NextStep,
                        wake_required: false,
                    },
                )
                .unwrap()
            })
            .collect();
        let page = StoreControlPage {
            after_seq: 0,
            records,
            durable_seq: 65,
        };

        assert!(matches!(
            page.validate(),
            Err(StoreError::Corrupt(message)) if message.contains("control page exceeds")
        ));
    }
}

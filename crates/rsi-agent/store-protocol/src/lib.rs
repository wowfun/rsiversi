//! Mechanical durable Agent Store seam.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    MAXIMUM_FACTS_PER_READ, SessionFact, SessionHeader, SessionId, TurnId, validate_fact_sequence,
};
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Exact `SQLite` and in-memory Store schema version.
pub const AGENT_STORE_SCHEMA_VERSION: u32 = 6;
/// Maximum Facts in one atomic append.
pub const MAXIMUM_STORE_BATCH_FACTS: usize = 512;
/// Maximum encoded bytes in one atomic append.
pub const MAXIMUM_STORE_BATCH_BYTES: usize = 64 * 1024 * 1024;
/// Maximum aggregate encoded bytes returned by one Fact page.
pub const MAXIMUM_STORE_FACT_PAGE_BYTES: usize = MAXIMUM_STORE_BATCH_BYTES;
/// Maximum immutable CAS object size.
pub const MAXIMUM_STORE_CAS_BYTES: usize = 64 * 1024 * 1024;
/// Maximum durable session identities in one enumeration page.
pub const MAXIMUM_SESSIONS_PER_READ: usize = 256;
/// Maximum opaque Context checkpoint bytes retained for one session.
pub const MAXIMUM_CONTEXT_CHECKPOINT_BYTES: usize =
    rsi_agent_session_protocol::MAXIMUM_CONTEXT_CHECKPOINT_BYTES;

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
    /// Reads the immutable header or returns not found.
    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader>;
    /// Reads at most `limit` contiguous Facts after one cursor.
    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage>;
    /// Reads at most `limit` ordered Facts for one exact turn after a session cursor.
    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreTurnFactPage>;
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
    /// Lists at most `limit` sessions containing at least one open turn after
    /// an exclusive lexical cursor.
    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage>;
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
}

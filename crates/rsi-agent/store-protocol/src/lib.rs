//! Mechanical durable Agent Store seam.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    MAXIMUM_FACTS_PER_READ, SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId,
    validate_fact_sequence,
};
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Exact `SQLite` and in-memory Store schema version.
pub const AGENT_STORE_SCHEMA_VERSION: u32 = 7;
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
        SessionFactBody::TurnAccepted { .. } | SessionFactBody::ImageRequested { .. } => {
            StoreFactTurnRole::Acceptance
        }
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
                SessionFactBody::TurnAccepted { .. } | SessionFactBody::ImageRequested { .. }
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
    /// Reads the immutable header or returns not found.
    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader>;
    /// Reads at most `limit` contiguous Facts after one cursor.
    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage>;
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
}

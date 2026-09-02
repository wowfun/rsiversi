//! Deterministic Agent protocol and plugin fixtures.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    EMPTY_FACT_PREFIX_DIGEST, SessionFact, SessionHeader, SessionId, TurnId,
    advance_fact_prefix_digest, fact_prefix_sha256,
};
use rsi_agent_store_protocol::{
    AppendBatch, AppendCommit, CasObjectRef, MAXIMUM_STORE_CAS_BYTES,
    MAXIMUM_STORE_FACT_PAGE_BYTES, Result, SessionStore, SessionStoreContract,
    StoreBackwardFactPage, StoreError, StoreFactPage, StoreFactTurnRole, StoreOpenTurn,
    StoreOpenTurnPage, StoreRecentSession, StoreRecentSessionCursor, StoreRecentSessionPage,
    StoreSessionPage, StoreTurnBoundary, StoreTurnFactPage, StoredContextCheckpoint,
    WriteContextCheckpoint, validate_read_limit, validate_session_read_limit,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Deterministic in-memory implementation of the mechanical Store seam.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<MemoryState>,
    fail_appends: AtomicUsize,
}

#[derive(Debug, Default)]
struct MemoryState {
    sessions: BTreeMap<SessionId, MemorySession>,
    recent_sessions: BTreeSet<(u64, SessionId)>,
    cas: BTreeMap<String, Arc<[u8]>>,
    fact_read_cursors: Vec<u64>,
}

#[derive(Debug)]
struct MemorySession {
    header: SessionHeader,
    facts: Vec<SessionFact>,
    turns: BTreeMap<TurnId, MemoryTurnBoundary>,
    fact_prefix_digest: [u8; 32],
    checkpoint: Option<StoredContextCheckpoint>,
}

#[derive(Clone, Debug)]
struct MemoryTurnBoundary {
    accepted_seq: u64,
    terminal_seq: Option<u64>,
}

impl MemoryStore {
    /// Creates an empty deterministic Store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes exactly the next `count` append attempts fail before mutation.
    pub fn fail_next_appends(&self, count: usize) {
        self.fail_appends.store(count, Ordering::Release);
    }

    /// Drains the exact cursors supplied to durable whole-session Fact reads.
    pub fn take_fact_read_cursors(&self) -> Vec<u64> {
        std::mem::take(
            &mut self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .fact_read_cursors,
        )
    }

    fn should_fail_append(&self) -> bool {
        self.fail_appends
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }
}

/// Exercises the backend-independent observable Store contract against one
/// empty implementation.
///
/// # Panics
///
/// Panics when the supplied fixture is internally inconsistent or the backend
/// violates any observable part of the mechanical Store contract.
#[allow(clippy::too_many_lines)]
pub async fn assert_mechanical_store_contract(
    store: &dyn SessionStore,
    header: SessionHeader,
    accepted: SessionFact,
    event: SessionFact,
    terminal: SessionFact,
) {
    let session_id = header.session_id().clone();
    let turn_id = accepted.body().turn_id().clone();
    assert_eq!(accepted.seq(), 1);
    assert_eq!(event.seq(), 2);
    assert_eq!(terminal.seq(), 3);
    assert_eq!(event.body().turn_id(), &turn_id);
    assert_eq!(terminal.body().turn_id(), &turn_id);

    let commit = store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![accepted.clone()],
        })
        .await
        .expect("create session");
    assert_eq!(commit.durable_seq, 1);
    assert_eq!(store.header(&session_id).await.unwrap(), header);
    assert!(matches!(
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: None,
                facts: vec![accepted.clone()],
            })
            .await,
        Err(StoreError::Conflict {
            expected: 0,
            actual: 1
        })
    ));

    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 1,
            header: None,
            facts: vec![event.clone()],
        })
        .await
        .expect("append open-turn event");
    let first = store.read_facts(&session_id, 0, 1).await.unwrap();
    assert_eq!(first.facts, vec![accepted.clone()]);
    assert!(!first.caught_up());
    let second = store.read_facts(&session_id, 1, 1).await.unwrap();
    assert_eq!(second.facts, vec![event.clone()]);
    assert!(second.caught_up());
    let newest = store.read_facts_before(&session_id, 0, 1).await.unwrap();
    assert_eq!(newest.before_seq, 3);
    assert_eq!(newest.facts, vec![event.clone()]);
    assert!(newest.has_more);
    let oldest = store
        .read_facts_before(&session_id, event.seq(), 1)
        .await
        .unwrap();
    assert_eq!(oldest.facts, vec![accepted.clone()]);
    assert!(!oldest.has_more);
    let turn = store
        .read_turn_facts(&session_id, &turn_id, 0, 8)
        .await
        .unwrap();
    assert_eq!(turn.facts, vec![accepted.clone(), event.clone()]);
    assert!(!turn.has_more);
    let open_boundary = store
        .read_turn_boundary(&session_id, &turn_id)
        .await
        .unwrap();
    assert_eq!(open_boundary.turn_id(), &turn_id);
    assert_eq!(open_boundary.accepted(), &accepted);
    assert_eq!(open_boundary.terminal(), None);
    assert_eq!(open_boundary.durable_seq(), 2);
    assert!(matches!(
        store
            .read_turn_boundary(&session_id, &TurnId::new("turn-absent").unwrap())
            .await,
        Err(StoreError::TurnNotFound { .. })
    ));
    let open = store.list_open_turns(&session_id, 0, 8).await.unwrap();
    assert_eq!(open.turns.len(), 1);
    assert_eq!(open.turns[0].turn_id, turn_id);
    let open_sessions = store.list_open_sessions(None, 8).await.unwrap();
    assert_eq!(open_sessions.sessions, vec![session_id.clone()]);
    assert!(!open_sessions.has_more);

    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 2,
            header: None,
            facts: vec![terminal.clone()],
        })
        .await
        .expect("close turn");
    assert!(
        store
            .list_open_turns(&session_id, 0, 8)
            .await
            .unwrap()
            .turns
            .is_empty()
    );
    let closed_boundary = store
        .read_turn_boundary(&session_id, &turn_id)
        .await
        .unwrap();
    assert_eq!(closed_boundary.turn_id(), &turn_id);
    assert_eq!(closed_boundary.accepted(), &accepted);
    assert_eq!(closed_boundary.terminal(), Some(&terminal));
    assert_eq!(closed_boundary.durable_seq(), 3);
    assert!(matches!(
        store
            .write_context_checkpoint(WriteContextCheckpoint {
                session_id: session_id.clone(),
                expected_durable_seq: 3,
                checkpoint: StoredContextCheckpoint {
                    header_fingerprint: header.fingerprint().unwrap(),
                    through_seq: 3,
                    fact_prefix_sha256: "b".repeat(64),
                    bytes: Arc::from(b"self-consistent-forged-checkpoint".as_slice()),
                },
            })
            .await,
        Err(StoreError::Invalid(_))
    ));
    let checkpoint = StoredContextCheckpoint {
        header_fingerprint: header.fingerprint().unwrap(),
        through_seq: 3,
        fact_prefix_sha256: fact_prefix_sha256([&accepted, &event, &terminal]).unwrap(),
        bytes: Arc::from(b"context-checkpoint-v2".as_slice()),
    };
    store
        .write_context_checkpoint(WriteContextCheckpoint {
            session_id: session_id.clone(),
            expected_durable_seq: 3,
            checkpoint: checkpoint.clone(),
        })
        .await
        .expect("write terminal-tail checkpoint");
    assert_eq!(
        store.read_context_checkpoint(&session_id).await.unwrap(),
        Some(checkpoint)
    );
    let replacement = StoredContextCheckpoint {
        header_fingerprint: header.fingerprint().unwrap(),
        through_seq: 3,
        fact_prefix_sha256: fact_prefix_sha256([&accepted, &event, &terminal]).unwrap(),
        bytes: Arc::from(b"context-checkpoint-v2-replacement".as_slice()),
    };
    store
        .write_context_checkpoint(WriteContextCheckpoint {
            session_id: session_id.clone(),
            expected_durable_seq: 3,
            checkpoint: replacement.clone(),
        })
        .await
        .expect("replace terminal-tail checkpoint");
    assert_eq!(
        store.read_context_checkpoint(&session_id).await.unwrap(),
        Some(replacement)
    );
    assert!(matches!(
        store
            .write_context_checkpoint(WriteContextCheckpoint {
                session_id: session_id.clone(),
                expected_durable_seq: 2,
                checkpoint: StoredContextCheckpoint {
                    header_fingerprint: header.fingerprint().unwrap(),
                    through_seq: 2,
                    fact_prefix_sha256: "c".repeat(64),
                    bytes: Arc::from(b"stale-checkpoint-v2".as_slice()),
                },
            })
            .await,
        Err(StoreError::Conflict {
            expected: 2,
            actual: 3
        })
    ));
    let sessions = store.list_sessions(None, 8).await.unwrap();
    assert_eq!(sessions.sessions, vec![session_id.clone()]);
    assert!(!sessions.has_more);
    let recent = store.list_recent_sessions(None, 8).await.unwrap();
    assert_eq!(recent.sessions.len(), 1);
    assert_eq!(recent.sessions[0].header, header);
    assert!(
        store
            .list_recent_sessions(Some(&recent.sessions[0].cursor()), 8)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    let closed_sessions = store.list_open_sessions(None, 8).await.unwrap();
    assert!(closed_sessions.sessions.is_empty());
    assert!(!closed_sessions.has_more);

    let bytes: Arc<[u8]> = Arc::from(b"shared Store contract".as_slice());
    let object = store.put_cas(Arc::clone(&bytes)).await.unwrap();
    assert_eq!(store.put_cas(Arc::clone(&bytes)).await.unwrap(), object);
    assert_eq!(store.read_cas(&object).await.unwrap(), bytes);
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn append(&self, batch: AppendBatch) -> Result<AppendCommit> {
        batch.validate()?;
        if self.should_fail_append() {
            return Err(StoreError::Io("injected append failure".into()));
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = state.sessions.get_mut(&batch.session_id) {
            let actual = session.facts.last().map_or(0, SessionFact::seq);
            if actual != batch.expected_seq {
                return Err(StoreError::Conflict {
                    expected: batch.expected_seq,
                    actual,
                });
            }
            if batch.header.is_some() {
                return Err(StoreError::Invalid(
                    "existing session cannot replace its immutable header".into(),
                ));
            }
            let turn_updates = index_appended_turns(&session.turns, &batch.facts)?;
            let fact_prefix_digest =
                batch
                    .facts
                    .iter()
                    .try_fold(session.fact_prefix_digest, |digest, fact| {
                        advance_fact_prefix_digest(digest, fact)
                            .map_err(|error| StoreError::Invalid(error.to_string()))
                    })?;
            session.facts.extend(batch.facts);
            session.turns.extend(turn_updates);
            session.fact_prefix_digest = fact_prefix_digest;
            Ok(AppendCommit {
                durable_seq: session
                    .facts
                    .last()
                    .expect("a validated append is nonempty")
                    .seq(),
            })
        } else {
            if batch.expected_seq != 0 {
                return Err(StoreError::Conflict {
                    expected: batch.expected_seq,
                    actual: 0,
                });
            }
            let header = batch
                .header
                .ok_or_else(|| StoreError::NotFound(batch.session_id.as_str().to_owned()))?;
            let durable_seq = batch
                .facts
                .last()
                .expect("a validated append is nonempty")
                .seq();
            let turns = index_appended_turns(&BTreeMap::new(), &batch.facts)?;
            let fact_prefix_digest =
                batch
                    .facts
                    .iter()
                    .try_fold(EMPTY_FACT_PREFIX_DIGEST, |digest, fact| {
                        advance_fact_prefix_digest(digest, fact)
                            .map_err(|error| StoreError::Invalid(error.to_string()))
                    })?;
            state
                .recent_sessions
                .insert((header.created_at_ms(), batch.session_id.clone()));
            state.sessions.insert(
                batch.session_id,
                MemorySession {
                    header,
                    facts: batch.facts,
                    turns,
                    fact_prefix_digest,
                    checkpoint: None,
                },
            );
            Ok(AppendCommit { durable_seq })
        }
    }

    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(session_id)
            .map(|session| session.header.clone())
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))
    }

    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage> {
        validate_read_limit(limit)?;
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.fact_read_cursors.push(after_seq);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, SessionFact::seq);
        if after_seq > durable_seq {
            return Err(StoreError::Invalid(
                "Fact cursor exceeds the durable tail".into(),
            ));
        }
        let start = usize::try_from(after_seq)
            .map_err(|_| StoreError::Invalid("Fact cursor does not fit memory".into()))?;
        let mut facts = Vec::new();
        let mut encoded_bytes = 0_usize;
        for fact in session.facts.iter().skip(start).take(limit) {
            let projected = encoded_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("Fact page size overflow".into()))?;
            if !facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES {
                break;
            }
            encoded_bytes = projected;
            facts.push(fact.clone());
        }
        let page = StoreFactPage {
            after_seq,
            facts,
            durable_seq,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> Result<StoreBackwardFactPage> {
        validate_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, SessionFact::seq);
        let maximum_before = durable_seq
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("durable sequence is exhausted".into()))?;
        let before_seq = if exclusive_before_seq == 0 {
            maximum_before
        } else {
            exclusive_before_seq
        };
        if before_seq > maximum_before {
            return Err(StoreError::Invalid(
                "backward Fact cursor exceeds one past the durable tail".into(),
            ));
        }
        let take = usize::try_from(before_seq - 1)
            .map_err(|_| StoreError::Invalid("Fact cursor does not fit memory".into()))?;
        let mut facts = Vec::new();
        let mut encoded_bytes = 0_usize;
        for fact in session.facts.iter().take(take).rev() {
            let projected = encoded_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("backward Fact page size overflow".into()))?;
            if facts.len() == limit
                || (!facts.is_empty() && projected > MAXIMUM_STORE_FACT_PAGE_BYTES)
            {
                break;
            }
            encoded_bytes = projected;
            facts.push(fact.clone());
        }
        facts.reverse();
        let has_more = facts.first().is_some_and(|fact| fact.seq() > 1);
        let page = StoreBackwardFactPage {
            before_seq,
            facts,
            durable_seq,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreTurnFactPage> {
        validate_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, SessionFact::seq);
        if after_seq > durable_seq {
            return Err(StoreError::Invalid(
                "turn Fact cursor exceeds the durable tail".into(),
            ));
        }
        if !session.turns.contains_key(turn_id) {
            return Err(StoreError::TurnNotFound {
                session: session_id.to_string(),
                turn: turn_id.to_string(),
            });
        }
        let mut facts = Vec::new();
        let mut encoded_bytes = 0_usize;
        let mut has_more = false;
        for fact in session
            .facts
            .iter()
            .filter(|fact| fact.seq() > after_seq && fact.body().turn_id() == turn_id)
        {
            let projected = encoded_bytes
                .checked_add(fact.encoded_len())
                .ok_or_else(|| StoreError::Corrupt("turn Fact page size overflow".into()))?;
            if facts.len() == limit
                || (!facts.is_empty()
                    && projected > rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES)
            {
                has_more = true;
                break;
            }
            encoded_bytes = projected;
            facts.push(fact.clone());
        }
        let page = StoreTurnFactPage {
            turn_id: turn_id.clone(),
            after_seq,
            facts,
            durable_seq,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> Result<StoreTurnBoundary> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let boundary = session
            .turns
            .get(turn_id)
            .ok_or_else(|| StoreError::TurnNotFound {
                session: session_id.to_string(),
                turn: turn_id.to_string(),
            })?;
        let accepted = session
            .facts
            .get(usize::try_from(boundary.accepted_seq - 1).expect("bounded sequence"))
            .expect("turn index acceptance points into Facts");
        let terminal = boundary.terminal_seq.map(|seq| {
            session
                .facts
                .get(usize::try_from(seq - 1).expect("bounded sequence"))
                .expect("turn index terminal points into Facts")
                .clone()
        });
        StoreTurnBoundary::new(
            turn_id.clone(),
            accepted.clone(),
            terminal,
            session.facts.last().map_or(0, SessionFact::seq),
        )
    }

    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> Result<StoreOpenTurnPage> {
        validate_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get(session_id)
            .ok_or_else(|| StoreError::NotFound(session_id.as_str().into()))?;
        let durable_seq = session.facts.last().map_or(0, SessionFact::seq);
        if after_accepted_seq > durable_seq {
            return Err(StoreError::Invalid(
                "open-turn cursor exceeds the durable tail".into(),
            ));
        }
        let mut turns = session
            .turns
            .iter()
            .filter_map(|(turn_id, boundary)| {
                (boundary.terminal_seq.is_none() && boundary.accepted_seq > after_accepted_seq)
                    .then_some(StoreOpenTurn {
                        turn_id: turn_id.clone(),
                        accepted_seq: boundary.accepted_seq,
                    })
            })
            .collect::<Vec<_>>();
        turns.sort_by_key(|turn| turn.accepted_seq);
        let has_more = turns.len() > limit;
        turns.truncate(limit);
        let page = StoreOpenTurnPage {
            after_accepted_seq,
            turns,
            durable_seq,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = state
            .sessions
            .keys()
            .filter(|session| after.is_none_or(|after| *session > after))
            .take(limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let page = StoreSessionPage {
            after: after.cloned(),
            sessions,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn list_recent_sessions(
        &self,
        after: Option<&StoreRecentSessionCursor>,
        limit: usize,
    ) -> Result<StoreRecentSessionPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = state
            .recent_sessions
            .iter()
            .rev()
            .filter(|(created_at_ms, session_id)| {
                after.is_none_or(|after| {
                    (*created_at_ms, session_id) < (after.created_at_ms, &after.session_id)
                })
            })
            .take(limit + 1)
            .map(|(_, session_id)| StoreRecentSession {
                header: state
                    .sessions
                    .get(session_id)
                    .expect("recent index references its Session")
                    .header
                    .clone(),
            })
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let page = StoreRecentSessionPage {
            after: after.cloned(),
            sessions,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage> {
        validate_session_read_limit(limit)?;
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sessions = state
            .sessions
            .iter()
            .filter(|(session_id, session)| {
                after.is_none_or(|after| *session_id > after)
                    && session
                        .turns
                        .values()
                        .any(|boundary| boundary.terminal_seq.is_none())
            })
            .map(|(session_id, _)| session_id.clone())
            .take(limit + 1)
            .collect::<Vec<_>>();
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let page = StoreSessionPage {
            after: after.cloned(),
            sessions,
            has_more,
        };
        page.validate()?;
        Ok(page)
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<StoredContextCheckpoint>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .get(session_id)
            .map(|session| session.checkpoint.clone())
            .ok_or_else(|| StoreError::NotFound(session_id.to_string()))
    }

    async fn write_context_checkpoint(&self, write: WriteContextCheckpoint) -> Result<()> {
        write.validate()?;
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = state
            .sessions
            .get_mut(&write.session_id)
            .ok_or_else(|| StoreError::NotFound(write.session_id.to_string()))?;
        let actual = session.facts.last().map_or(0, SessionFact::seq);
        if actual != write.expected_durable_seq {
            return Err(StoreError::Conflict {
                expected: write.expected_durable_seq,
                actual,
            });
        }
        if write.checkpoint.header_fingerprint
            != session.header.fingerprint().map_err(|error| {
                StoreError::Corrupt(format!("stored session header is invalid: {error}"))
            })?
        {
            return Err(StoreError::Invalid(
                "checkpoint header fingerprint differs from the durable session".into(),
            ));
        }
        if write.checkpoint.fact_prefix_sha256 != hex::encode(session.fact_prefix_digest) {
            return Err(StoreError::Invalid(
                "checkpoint Fact-prefix digest differs from the durable session".into(),
            ));
        }
        session.checkpoint = Some(write.checkpoint);
        Ok(())
    }

    async fn put_cas(&self, bytes: Arc<[u8]>) -> Result<CasObjectRef> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_STORE_CAS_BYTES {
            return Err(StoreError::Invalid(
                "CAS bytes must be nonempty and bounded".into(),
            ));
        }
        let reference = CasObjectRef {
            sha256: hex::encode(Sha256::digest(&bytes)),
            byte_len: bytes.len() as u64,
        };
        reference.validate()?;
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cas
            .entry(reference.sha256.clone())
            .or_insert(bytes);
        Ok(reference)
    }

    async fn read_cas(&self, object: &CasObjectRef) -> Result<Arc<[u8]>> {
        object.validate()?;
        let bytes = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cas
            .get(&object.sha256)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(object.sha256.clone()))?;
        if bytes.len() as u64 != object.byte_len
            || hex::encode(Sha256::digest(&bytes)) != object.sha256
        {
            return Err(StoreError::Corrupt(
                "CAS bytes do not match their reference".into(),
            ));
        }
        Ok(bytes)
    }
}

fn index_appended_turns(
    turns: &BTreeMap<TurnId, MemoryTurnBoundary>,
    facts: &[SessionFact],
) -> Result<BTreeMap<TurnId, MemoryTurnBoundary>> {
    let mut updates = BTreeMap::new();
    for fact in facts {
        let role = rsi_agent_store_protocol::store_fact_turn_role(fact.body());
        let turn_id = fact.body().turn_id();
        match role {
            StoreFactTurnRole::Acceptance => {
                if turns.contains_key(turn_id) || updates.contains_key(turn_id) {
                    return Err(StoreError::Corrupt(role.rejected_message().into()));
                }
                updates.insert(
                    turn_id.clone(),
                    MemoryTurnBoundary {
                        accepted_seq: fact.seq(),
                        terminal_seq: None,
                    },
                );
            }
            StoreFactTurnRole::Terminal => {
                if !updates.contains_key(turn_id) {
                    let boundary = turns
                        .get(turn_id)
                        .cloned()
                        .ok_or_else(|| StoreError::Corrupt(role.rejected_message().into()))?;
                    updates.insert(turn_id.clone(), boundary);
                }
                let boundary = updates.get_mut(turn_id).expect("boundary was inserted");
                if boundary.terminal_seq.is_some() {
                    return Err(StoreError::Corrupt(role.rejected_message().into()));
                }
                boundary.terminal_seq = Some(fact.seq());
            }
            StoreFactTurnRole::Event => {
                if updates
                    .get(turn_id)
                    .or_else(|| turns.get(turn_id))
                    .is_none_or(|boundary| boundary.terminal_seq.is_some())
                {
                    return Err(StoreError::Corrupt(role.rejected_message().into()));
                }
            }
        }
    }
    Ok(updates)
}

/// Test-only ordinary factory providing one chosen Memory Store instance.
#[derive(Clone, Debug)]
pub struct MemoryStoreFactory {
    store: Arc<MemoryStore>,
}

impl MemoryStoreFactory {
    /// Creates a factory around an observable Store fixture.
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl PluginFactory for MemoryStoreFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Memory Agent Store configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let store: Arc<dyn SessionStore> = self.store.clone();
        let supply = plan
            .context()
            .provide_local::<SessionStoreContract>(store)?;
        plan.defer(
            "withdraw Memory Agent Store",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

//! Deterministic Agent protocol and plugin fixtures.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionFact, SessionFactBody, SessionHeader, SessionId, TurnId};
use rsi_agent_store_protocol::{
    AppendBatch, AppendCommit, CasObjectRef, MAXIMUM_STORE_CAS_BYTES,
    MAXIMUM_STORE_FACT_PAGE_BYTES, Result, SessionStore, SessionStoreContract, StoreError,
    StoreFactPage, StoreOpenTurn, StoreOpenTurnPage, StoreSessionPage, StoreTurnFactPage,
    validate_read_limit, validate_session_read_limit,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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
    cas: BTreeMap<String, Arc<[u8]>>,
}

#[derive(Debug)]
struct MemorySession {
    header: SessionHeader,
    facts: Vec<SessionFact>,
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
    let turn = store
        .read_turn_facts(&session_id, &turn_id, 0, 8)
        .await
        .unwrap();
    assert_eq!(turn.facts, vec![accepted, event]);
    assert!(!turn.has_more);
    let open = store.list_open_turns(&session_id, 0, 8).await.unwrap();
    assert_eq!(open.turns.len(), 1);
    assert_eq!(open.turns[0].turn_id, turn_id);

    store
        .append(AppendBatch {
            session_id: session_id.clone(),
            expected_seq: 2,
            header: None,
            facts: vec![terminal],
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
    let sessions = store.list_sessions(None, 8).await.unwrap();
    assert_eq!(sessions.sessions, vec![session_id]);
    assert!(!sessions.has_more);

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
            index_turn_lifecycle(session.facts.iter().chain(batch.facts.iter()))?;
            session.facts.extend(batch.facts);
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
            index_turn_lifecycle(&batch.facts)?;
            state.sessions.insert(
                batch.session_id,
                MemorySession {
                    header,
                    facts: batch.facts,
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
        if !session
            .facts
            .iter()
            .any(|fact| fact.body().turn_id() == turn_id)
        {
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
        let lifecycle = index_turn_lifecycle(&session.facts)?;
        let mut turns = lifecycle
            .into_iter()
            .filter_map(|(turn_id, (accepted_seq, open))| {
                (open && accepted_seq > after_accepted_seq).then_some(StoreOpenTurn {
                    turn_id,
                    accepted_seq,
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

fn index_turn_lifecycle<'a>(
    facts: impl IntoIterator<Item = &'a SessionFact>,
) -> Result<BTreeMap<TurnId, (u64, bool)>> {
    let mut lifecycle = BTreeMap::<TurnId, (u64, bool)>::new();
    for fact in facts {
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, .. }
            | SessionFactBody::ImageRequested { turn_id, .. } => {
                if lifecycle
                    .insert(turn_id.clone(), (fact.seq(), true))
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "durable turn was accepted more than once".into(),
                    ));
                }
            }
            SessionFactBody::TurnTerminal { turn_id, .. } => {
                let (_, open) = lifecycle.get_mut(turn_id).ok_or_else(|| {
                    StoreError::Corrupt("terminal references a closed or unknown turn".into())
                })?;
                if !*open {
                    return Err(StoreError::Corrupt(
                        "terminal references a closed or unknown turn".into(),
                    ));
                }
                *open = false;
            }
            body => {
                if !lifecycle.get(body.turn_id()).is_some_and(|(_, open)| *open) {
                    return Err(StoreError::Corrupt(
                        "nonterminal Fact references a closed or unknown turn".into(),
                    ));
                }
            }
        }
    }
    Ok(lifecycle)
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

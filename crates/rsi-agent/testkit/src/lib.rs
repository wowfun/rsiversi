//! Deterministic Agent protocol and plugin fixtures.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_agent_session_protocol::{
    ActivationId, ActivationOutcome, AgentControlRecord, AgentControlRecordBody, AgentMessage,
    AgentMessageContent, AgentMessageSource, AgentPath, EMPTY_CONTROL_PREFIX_DIGEST,
    EMPTY_FACT_PREFIX_DIGEST, ForkOrigin, ForkTurnSelection, InputMessageSource, MessageId,
    MessageOptions, MessageTarget, SessionFact, SessionFactBody, SessionHeader, SessionId, StepId,
    TurnId, TurnOutcome, advance_control_prefix_digest, advance_fact_prefix_digest,
    fact_prefix_sha256,
};
use rsi_agent_store_protocol::{
    AgentActivationGuard, AgentCommitWatermark, AppendBatch, AppendCommit, AtomicAgentCommit,
    AtomicAgentCommitResult, AtomicSessionAppend, CasObjectRef, MAXIMUM_STORE_BATCH_FACTS,
    MAXIMUM_STORE_CAS_BYTES, MAXIMUM_STORE_CONTROL_PAGE_BYTES, MAXIMUM_STORE_FACT_PAGE_BYTES,
    MAXIMUM_STORE_MAILBOX_PAGE_BYTES, Result, SessionStore, SessionStoreContract,
    StoreActivationPhase, StoreActiveActivation, StoreAgentChild, StoreAgentChildPage,
    StoreAgentDescendantStatus, StoreAgentMailbox, StoreAgentMailboxSummary, StoreAgentMessage,
    StoreAgentMessageState, StoreAgentSessionStatus, StoreAgentSubtreeSnapshot,
    StoreBackwardFactPage, StoreControlPage, StoreError, StoreFactPage, StoreFactTurnRole,
    StoreForkBoundary, StoreOpenTurn, StoreOpenTurnPage, StoreReadyMessage,
    StoreReadyMessageCursor, StoreReadyMessagePage, StoreReadyRootPage, StoreRecentSession,
    StoreRecentSessionCursor, StoreRecentSessionPage, StoreSessionPage, StoreTurnBoundary,
    StoreTurnFactPage, StoreWaitingActivationPage, StoreWorkspaceContextState,
    StoredContextCheckpoint, WriteContextCheckpoint, validate_message_claim_fact,
    validate_read_limit, validate_session_read_limit,
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
    fork_boundary_resolutions: AtomicUsize,
    fail_agent_children: Mutex<Option<SessionId>>,
}

#[derive(Clone, Debug, Default)]
struct MemoryState {
    sessions: BTreeMap<SessionId, MemorySession>,
    recent_sessions: BTreeSet<(u64, SessionId)>,
    cas: BTreeMap<String, Arc<[u8]>>,
    fact_read_cursors: Vec<u64>,
    ready_messages: BTreeMap<(SessionId, u64, SessionId, u64), StoreReadyMessage>,
    ready_keys: BTreeMap<(SessionId, MessageId), (SessionId, u64, SessionId, u64)>,
    agent_messages: BTreeMap<(SessionId, MessageId), StoreAgentMessage>,
    agent_children: BTreeMap<(SessionId, SessionId), StoreAgentChild>,
    active_activations: BTreeMap<SessionId, StoreActiveActivation>,
}

#[derive(Clone, Debug)]
struct MemorySession {
    header: SessionHeader,
    facts: Vec<Arc<SessionFact>>,
    turns: BTreeMap<TurnId, MemoryTurnBoundary>,
    fact_prefix_digest: [u8; 32],
    checkpoint: Option<StoredContextCheckpoint>,
    controls: Vec<AgentControlRecord>,
    control_prefix_digest: [u8; 32],
    workspace_context: StoreWorkspaceContextState,
}

#[derive(Clone, Debug)]
struct MemoryTurnBoundary {
    accepted_seq: u64,
    terminal_seq: Option<u64>,
    terminal_prefix_sha256: Option<String>,
    terminal_control: Option<(u64, String)>,
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

    /// Returns how many immutable fork-boundary resolutions this fixture served.
    pub fn fork_boundary_resolution_count(&self) -> usize {
        self.fork_boundary_resolutions.load(Ordering::Acquire)
    }

    /// Makes the next child-list or descendant-snapshot read fail for one exact parent.
    pub fn fail_next_agent_tree_read_for(&self, parent_session_id: SessionId) {
        *self
            .fail_agent_children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(parent_session_id);
    }

    fn should_fail_append(&self) -> bool {
        self.fail_appends
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    fn should_fail_agent_tree_read(&self, parent_session_id: &SessionId) -> bool {
        let mut failure = self
            .fail_agent_children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure.as_ref() == Some(parent_session_id) {
            failure.take();
            true
        } else {
            false
        }
    }
}

mod contribution;
mod memory_store;
mod store_contract;

pub use contribution::activate_contribution_owner;
pub use store_contract::assert_mechanical_store_contract;

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

/// Seeds explicit test history through the current canonical terminal commit contract.
/// Each terminal closes its own transaction; this helper is not an atomic batch API.
pub async fn append_history_fixture(
    store: &dyn SessionStore,
    batch: AppendBatch,
) -> Result<AppendCommit> {
    let mut expected_seq = batch.expected_seq;
    let mut header = batch.header;
    let mut pending = Vec::new();
    let mut facts = batch.facts.into_iter().peekable();
    while let Some(fact) = facts.next() {
        let terminal = matches!(fact.body(), SessionFactBody::TurnTerminal { .. });
        pending.push(Arc::clone(&fact));
        if !terminal && facts.peek().is_some() && pending.len() < MAXIMUM_STORE_BATCH_FACTS {
            continue;
        }
        let through_seq = fact.seq();
        if terminal {
            let expected_control_seq = if header.is_some() {
                0
            } else {
                store
                    .read_controls(&batch.session_id, 0, 1)
                    .await?
                    .durable_seq
            };
            let terminal = &fact;
            let marker = AgentControlRecord::new(
                expected_control_seq.checked_add(1).ok_or_else(|| {
                    StoreError::Invalid("fixture control sequence exhausted".into())
                })?,
                terminal.timestamp_ms(),
                AgentControlRecordBody::TurnBoundaryRecorded {
                    turn_id: terminal.body().turn_id().clone(),
                    terminal_fact_seq: terminal.seq(),
                },
            )
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
            store
                .commit_agent(AtomicAgentCommit {
                    sessions: vec![AtomicSessionAppend {
                        session_id: batch.session_id.clone(),
                        expected_fact_seq: expected_seq,
                        expected_control_seq,
                        header: header.take(),
                        facts: std::mem::take(&mut pending),
                        controls: vec![marker],
                    }],
                    required_active_activations: Vec::new(),
                    quiescent_descendants_of: None,
                })
                .await?;
        } else {
            store
                .append(AppendBatch {
                    session_id: batch.session_id.clone(),
                    expected_seq,
                    header: header.take(),
                    facts: std::mem::take(&mut pending),
                })
                .await?;
        }
        expected_seq = through_seq;
    }
    Ok(AppendCommit {
        durable_seq: expected_seq,
    })
}

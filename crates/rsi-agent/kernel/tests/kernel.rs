use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentCompositionError, AgentCompositionPin,
    AgentSessionDraft, PreparedFreshSession,
};
use rsi_agent_kernel::{
    Clock, DEFAULT_MAXIMUM_ACTIVE_OBSERVERS, KernelFactory, KernelLimits, MAXIMUM_ACTIVE_SESSIONS,
    MAXIMUM_PENDING_FACT_BYTES, SessionKernel,
};
use rsi_agent_session_protocol::{
    ActivationId, AgentControlRecordBody, AgentMessage, AgentMessageContent, AgentMessageSource,
    AgentPath, AgentPresetId, BudgetDimension, EffectId, EffectKind, ForkTurnSelection,
    FrozenAgentSettings, MAXIMUM_AGENT_DIAGNOSTIC_BYTES, MAXIMUM_FACTS_PER_READ,
    MAXIMUM_SESSION_FACT_BYTES, MAXIMUM_TURN_GENERATED_FACT_BYTES, MAXIMUM_TURN_TEXT_BYTES,
    MessageId, MessageOptions, MessageTarget, SessionFact, SessionFactBody, SessionHeader,
    SessionId, StepId, TurnBudget, TurnId, TurnOutcome, WaitResumeCause, fact_prefix_sha256,
};
use rsi_agent_store_protocol::{
    AppendBatch, AppendCommit, CasObjectRef, SessionStore, StoreActivationPhase, StoreError,
    StoreFactPage, StoreOpenTurnPage, StoreSessionPage, StoreTurnBoundary, StoreTurnFactPage,
    StoreWaitingActivationPage, StoredContextCheckpoint, WriteContextCheckpoint,
};
use rsi_agent_testkit::{MemoryStore, MemoryStoreFactory};
use rsi_agent_turn_protocol::{
    AgentWaitResult, CancelTarget, ClaimMessage, ContextCheckpoint, MessageState,
    ObservationCursor, PublishAttempt, SendAgentMessage, SessionObservation, SpawnAgentRequest,
    SubmitMessage, SubmitSession, SubmitTurn, TurnClaimIssuer, TurnError, TurnExecution,
    TurnFinalizationContext, TurnFinalizationError, TurnFinalizationReport, TurnFinalizer,
    TurnService, TurnUpdate,
};
use rsi_agent_workspace_context::{
    WorkspaceContext, WorkspaceContextError, WorkspaceContextFactory, WorkspaceContextSnapshot,
};
use rsi_ai_protocol::{
    AiCapability, ContentDelta, ContentStart, LanguageEvent, MAX_LANGUAGE_OUTPUT_BYTES, ModelRef,
    PreparedCallSnapshot, RetryPolicy,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolError, ToolResultIdentity,
    ToolRuntime,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

trait PublishedFacts {
    fn published(self) -> Vec<Arc<SessionFact>>;
}

impl PublishedFacts for PublishAttempt {
    fn published(self) -> Vec<Arc<SessionFact>> {
        match self {
            PublishAttempt::Published(facts) => facts,
            PublishAttempt::FlushRequired { .. } => panic!("expected published Facts"),
        }
    }
}

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        42
    }
}

#[derive(Debug)]
struct FactReadRaceStore {
    inner: Arc<MemoryStore>,
    block_header_reads: AtomicBool,
    blocked_header_session: Mutex<Option<SessionId>>,
    header_read_attempts: AtomicUsize,
    release_header_reads: Notify,
    pause_read: AtomicBool,
    read_attempts: AtomicUsize,
    read_captured: Notify,
    release_read: Notify,
    read_error: Mutex<Option<String>>,
    pause_open_turn_read: AtomicBool,
    open_turn_read_attempts: AtomicUsize,
    open_turn_read_captured: Notify,
    release_open_turn_read: Notify,
    block_turn_boundary_reads: AtomicBool,
    turn_boundary_read_attempts: AtomicUsize,
    turn_boundary_read_started: Notify,
    release_turn_boundary_read: Notify,
    append_attempts: AtomicUsize,
    pause_append_at: AtomicUsize,
    append_blocked: Notify,
    release_append: Notify,
    pause_agent_commit_before_apply: AtomicBool,
    agent_commit_before_apply: Notify,
    release_agent_commit_before_apply: Notify,
    pause_agent_commit_after_apply: AtomicBool,
    agent_commit_applied: Notify,
    release_agent_commit: Notify,
    pause_descendant_snapshot_at: AtomicUsize,
    descendant_snapshot_reads: AtomicUsize,
    descendant_snapshot_blocked: Notify,
    release_descendant_snapshot: Notify,
    permanent_append_failure_for: Mutex<Option<SessionId>>,
    fail_checkpoint_write: AtomicBool,
    corrupt_next_ready_target: AtomicBool,
    fail_ready_roots: AtomicBool,
    active_recheck_mismatches: Mutex<Option<(SessionId, usize, bool)>>,
}

impl FactReadRaceStore {
    fn new(inner: Arc<MemoryStore>) -> Self {
        Self {
            inner,
            block_header_reads: AtomicBool::new(false),
            blocked_header_session: Mutex::new(None),
            header_read_attempts: AtomicUsize::new(0),
            release_header_reads: Notify::new(),
            pause_read: AtomicBool::new(false),
            read_attempts: AtomicUsize::new(0),
            read_captured: Notify::new(),
            release_read: Notify::new(),
            read_error: Mutex::new(None),
            pause_open_turn_read: AtomicBool::new(false),
            open_turn_read_attempts: AtomicUsize::new(0),
            open_turn_read_captured: Notify::new(),
            release_open_turn_read: Notify::new(),
            block_turn_boundary_reads: AtomicBool::new(false),
            turn_boundary_read_attempts: AtomicUsize::new(0),
            turn_boundary_read_started: Notify::new(),
            release_turn_boundary_read: Notify::new(),
            append_attempts: AtomicUsize::new(0),
            pause_append_at: AtomicUsize::new(0),
            append_blocked: Notify::new(),
            release_append: Notify::new(),
            pause_agent_commit_before_apply: AtomicBool::new(false),
            agent_commit_before_apply: Notify::new(),
            release_agent_commit_before_apply: Notify::new(),
            pause_agent_commit_after_apply: AtomicBool::new(false),
            agent_commit_applied: Notify::new(),
            release_agent_commit: Notify::new(),
            pause_descendant_snapshot_at: AtomicUsize::new(0),
            descendant_snapshot_reads: AtomicUsize::new(0),
            descendant_snapshot_blocked: Notify::new(),
            release_descendant_snapshot: Notify::new(),
            permanent_append_failure_for: Mutex::new(None),
            fail_checkpoint_write: AtomicBool::new(false),
            corrupt_next_ready_target: AtomicBool::new(false),
            fail_ready_roots: AtomicBool::new(false),
            active_recheck_mismatches: Mutex::new(None),
        }
    }

    fn block_header_reads(&self) {
        self.block_header_reads.store(true, Ordering::Release);
    }

    fn corrupt_next_ready_target(&self) {
        self.corrupt_next_ready_target
            .store(true, Ordering::Release);
    }

    fn mismatch_active_rechecks(&self, session_id: SessionId, count: usize) {
        *self
            .active_recheck_mismatches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((session_id, count, false));
    }

    fn pause_second_next_descendant_snapshot(&self) {
        self.pause_descendant_snapshot_at.store(
            self.descendant_snapshot_reads
                .load(Ordering::Acquire)
                .saturating_add(2),
            Ordering::Release,
        );
    }

    fn pause_next_descendant_snapshot(&self) {
        self.pause_descendant_snapshot_at.store(
            self.descendant_snapshot_reads
                .load(Ordering::Acquire)
                .saturating_add(1),
            Ordering::Release,
        );
    }

    async fn wait_for_descendant_snapshot_pause(&self) {
        self.descendant_snapshot_blocked.notified().await;
    }

    fn release_descendant_snapshot(&self) {
        self.release_descendant_snapshot.notify_waiters();
    }

    fn block_header_reads_for(&self, session_id: SessionId) {
        *self
            .blocked_header_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id);
        self.block_header_reads.store(true, Ordering::Release);
    }

    fn header_read_attempts(&self) -> usize {
        self.header_read_attempts.load(Ordering::Acquire)
    }

    fn reset_header_read_attempts(&self) {
        self.header_read_attempts.store(0, Ordering::Release);
    }

    fn release_blocked_header_reads(&self) {
        self.block_header_reads.store(false, Ordering::Release);
        self.blocked_header_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.release_header_reads.notify_waiters();
    }

    fn pause_next_read(&self) {
        self.pause_read.store(true, Ordering::Release);
    }

    fn reset_read_attempts(&self) {
        self.read_attempts.store(0, Ordering::Release);
    }

    fn read_attempts(&self) -> usize {
        self.read_attempts.load(Ordering::Acquire)
    }

    fn pause_next_open_turn_read(&self) {
        self.pause_open_turn_read.store(true, Ordering::Release);
    }

    fn reset_open_turn_read_attempts(&self) {
        self.open_turn_read_attempts.store(0, Ordering::Release);
    }

    fn open_turn_read_attempts(&self) -> usize {
        self.open_turn_read_attempts.load(Ordering::Acquire)
    }

    async fn wait_until_open_turn_read_is_captured(&self) {
        self.open_turn_read_captured.notified().await;
    }

    fn release_captured_open_turn_read(&self) {
        self.release_open_turn_read.notify_one();
    }

    async fn wait_until_read_is_captured(&self) {
        self.read_captured.notified().await;
    }

    fn release_captured_read(&self) {
        self.release_read.notify_one();
    }

    fn fail_next_read(&self, message: String) {
        *self
            .read_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message);
    }

    fn pause_second_following_append(&self) {
        let attempt = self.append_attempts.load(Ordering::Acquire) + 2;
        self.pause_append_at.store(attempt, Ordering::Release);
    }

    fn pause_next_append(&self) {
        let attempt = self.append_attempts.load(Ordering::Acquire) + 1;
        self.pause_append_at.store(attempt, Ordering::Release);
    }

    async fn wait_until_append_is_blocked(&self) {
        self.append_blocked.notified().await;
    }

    fn release_blocked_append(&self) {
        self.release_append.notify_one();
    }

    fn pause_next_agent_commit_before_apply(&self) {
        self.pause_agent_commit_before_apply
            .store(true, Ordering::Release);
    }

    async fn wait_until_agent_commit_is_before_apply(&self) {
        self.agent_commit_before_apply.notified().await;
    }

    fn release_agent_commit_before_apply(&self) {
        self.release_agent_commit_before_apply.notify_one();
    }

    fn pause_next_agent_commit_after_apply(&self) {
        self.pause_agent_commit_after_apply
            .store(true, Ordering::Release);
    }

    async fn wait_until_agent_commit_is_applied(&self) {
        self.agent_commit_applied.notified().await;
    }

    fn release_applied_agent_commit(&self) {
        self.release_agent_commit.notify_one();
    }

    fn fail_appends_for(&self, session_id: SessionId) {
        *self
            .permanent_append_failure_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id);
    }

    fn fail_next_checkpoint_write(&self) {
        self.fail_checkpoint_write.store(true, Ordering::Release);
    }

    fn block_turn_boundary_reads(&self) {
        self.block_turn_boundary_reads
            .store(true, Ordering::Release);
    }

    async fn wait_for_turn_boundary_attempts(&self, expected: usize) {
        while self.turn_boundary_read_attempts.load(Ordering::Acquire) < expected {
            self.turn_boundary_read_started.notified().await;
        }
    }

    fn turn_boundary_read_attempts(&self) -> usize {
        self.turn_boundary_read_attempts.load(Ordering::Acquire)
    }

    fn release_one_turn_boundary_read(&self) {
        self.release_turn_boundary_read.notify_one();
    }
}

#[async_trait]
impl SessionStore for FactReadRaceStore {
    async fn append(&self, batch: AppendBatch) -> rsi_agent_store_protocol::Result<AppendCommit> {
        let attempt = self.append_attempts.fetch_add(1, Ordering::AcqRel) + 1;
        if self.pause_append_at.load(Ordering::Acquire) == attempt {
            self.append_blocked.notify_one();
            self.release_append.notified().await;
        }
        if self
            .permanent_append_failure_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            == Some(&batch.session_id)
        {
            return Err(StoreError::Invalid(
                "injected permanent append failure".into(),
            ));
        }
        self.inner.append(batch).await
    }

    async fn commit_agent(
        &self,
        commit: rsi_agent_store_protocol::AtomicAgentCommit,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::AtomicAgentCommitResult> {
        if self
            .pause_agent_commit_before_apply
            .swap(false, Ordering::AcqRel)
        {
            self.agent_commit_before_apply.notify_one();
            self.release_agent_commit_before_apply.notified().await;
        }
        let result = self.inner.commit_agent(commit).await;
        if result.is_ok()
            && self
                .pause_agent_commit_after_apply
                .swap(false, Ordering::AcqRel)
        {
            self.agent_commit_applied.notify_one();
            self.release_agent_commit.notified().await;
        }
        result
    }

    async fn header(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<SessionHeader> {
        self.header_read_attempts.fetch_add(1, Ordering::AcqRel);
        let released = self.release_header_reads.notified();
        let blocked_session = self
            .blocked_header_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if self.block_header_reads.load(Ordering::Acquire)
            && blocked_session
                .as_ref()
                .is_none_or(|blocked| blocked == session_id)
        {
            released.await;
        }
        self.inner.header(session_id).await
    }

    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreFactPage> {
        self.read_attempts.fetch_add(1, Ordering::AcqRel);
        if let Some(message) = self
            .read_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return Err(StoreError::Io(message));
        }
        let page = self.inner.read_facts(session_id, after_seq, limit).await?;
        if self.pause_read.swap(false, Ordering::AcqRel) {
            self.read_captured.notify_one();
            self.release_read.notified().await;
        }
        Ok(page)
    }

    async fn read_controls(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreControlPage> {
        self.inner.read_controls(session_id, after_seq, limit).await
    }

    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreBackwardFactPage> {
        self.inner
            .read_facts_before(session_id, exclusive_before_seq, limit)
            .await
    }

    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreTurnFactPage> {
        self.inner
            .read_turn_facts(session_id, turn_id, after_seq, limit)
            .await
    }

    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> rsi_agent_store_protocol::Result<StoreTurnBoundary> {
        self.turn_boundary_read_attempts
            .fetch_add(1, Ordering::AcqRel);
        self.turn_boundary_read_started.notify_waiters();
        if self.block_turn_boundary_reads.load(Ordering::Acquire) {
            self.release_turn_boundary_read.notified().await;
        }
        self.inner.read_turn_boundary(session_id, turn_id).await
    }

    async fn resolve_fork_boundary(
        &self,
        session_id: &SessionId,
        invoking_turn_id: &TurnId,
        selection: ForkTurnSelection,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreForkBoundary> {
        self.inner
            .resolve_fork_boundary(session_id, invoking_turn_id, selection)
            .await
    }

    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreOpenTurnPage> {
        self.open_turn_read_attempts.fetch_add(1, Ordering::AcqRel);
        let page = self
            .inner
            .list_open_turns(session_id, after_accepted_seq, limit)
            .await?;
        if self.pause_open_turn_read.swap(false, Ordering::AcqRel) {
            self.open_turn_read_captured.notify_one();
            self.release_open_turn_read.notified().await;
        }
        Ok(page)
    }

    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreSessionPage> {
        self.inner.list_sessions(after, limit).await
    }

    async fn list_recent_sessions(
        &self,
        after: Option<&rsi_agent_store_protocol::StoreRecentSessionCursor>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreRecentSessionPage> {
        self.inner.list_recent_sessions(after, limit).await
    }

    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreSessionPage> {
        self.inner.list_open_sessions(after, limit).await
    }

    async fn list_ready_messages(
        &self,
        root_session_id: &SessionId,
        after: Option<&rsi_agent_store_protocol::StoreReadyMessageCursor>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreReadyMessagePage> {
        let mut page = self
            .inner
            .list_ready_messages(root_session_id, after, limit)
            .await?;
        if self.corrupt_next_ready_target.swap(false, Ordering::AcqRel)
            && let Some(message) = page.messages.first_mut()
        {
            message.target = MessageTarget::NextStep;
        }
        Ok(page)
    }

    async fn read_agent_mailbox(
        &self,
        session_id: &SessionId,
        selected_message_id: Option<&MessageId>,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreAgentMailbox> {
        self.inner
            .read_agent_mailbox(session_id, selected_message_id)
            .await
    }

    async fn read_agent_mailbox_summary(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreAgentMailboxSummary> {
        self.inner.read_agent_mailbox_summary(session_id).await
    }

    async fn read_workspace_context_state(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreWorkspaceContextState>
    {
        self.inner.read_workspace_context_state(session_id).await
    }

    async fn list_ready_roots(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreReadyRootPage> {
        if self.fail_ready_roots.swap(false, Ordering::AcqRel) {
            return Err(StoreError::Io("transient ready-root scan failure".into()));
        }
        self.inner.list_ready_roots(after, limit).await
    }

    async fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        after: Option<&SessionId>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreAgentChildPage> {
        self.inner
            .list_agent_children(parent_session_id, after, limit)
            .await
    }

    async fn read_descendant_control_snapshot(
        &self,
        parent_session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreDescendantControlSnapshot>
    {
        let attempt = self
            .descendant_snapshot_reads
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if self.pause_descendant_snapshot_at.load(Ordering::Acquire) == attempt {
            self.descendant_snapshot_blocked.notify_one();
            self.release_descendant_snapshot.notified().await;
        }
        self.inner
            .read_descendant_control_snapshot(parent_session_id)
            .await
    }

    async fn active_activation(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<Option<rsi_agent_store_protocol::StoreActiveActivation>>
    {
        let active = self.inner.active_activation(session_id).await?;
        let mut mismatches = self
            .active_recheck_mismatches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((target, remaining, awaiting_recheck)) = mismatches.as_mut()
            && target == session_id
            && *remaining > 0
            && active.as_ref().is_some_and(|activation| {
                activation.phase == StoreActivationPhase::WaitingForDescendants
            })
        {
            if *awaiting_recheck {
                *awaiting_recheck = false;
                *remaining -= 1;
                return Ok(None);
            }
            *awaiting_recheck = true;
        }
        Ok(active)
    }

    async fn completion_reservation_count(
        &self,
        parent_session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<usize> {
        self.inner
            .completion_reservation_count(parent_session_id)
            .await
    }

    async fn list_waiting_activations(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreWaitingActivationPage> {
        self.inner.list_waiting_activations(after, limit).await
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<Option<StoredContextCheckpoint>> {
        self.inner.read_context_checkpoint(session_id).await
    }

    async fn write_context_checkpoint(
        &self,
        write: WriteContextCheckpoint,
    ) -> rsi_agent_store_protocol::Result<()> {
        if self.fail_checkpoint_write.swap(false, Ordering::AcqRel) {
            return Err(StoreError::Io("injected checkpoint write failure".into()));
        }
        self.inner.write_context_checkpoint(write).await
    }

    async fn put_cas(&self, bytes: Arc<[u8]>) -> rsi_agent_store_protocol::Result<CasObjectRef> {
        self.inner.put_cas(bytes).await
    }

    async fn read_cas(&self, object: &CasObjectRef) -> rsi_agent_store_protocol::Result<Arc<[u8]>> {
        self.inner.read_cas(object).await
    }
}

#[derive(Debug)]
struct RecordingFinalizer {
    name: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
}

#[derive(Debug)]
struct CoordinatedFinalizer {
    entered: Arc<AtomicUsize>,
    entered_changed: Arc<Notify>,
    release: Arc<Notify>,
    fail: bool,
}

#[async_trait]
impl TurnFinalizer for CoordinatedFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        self.entered.fetch_add(1, Ordering::AcqRel);
        self.entered_changed.notify_waiters();
        self.release.notified().await;
        if self.fail {
            Err(TurnFinalizationError::Failed {
                code: "test.concurrent_failure".into(),
                message: "concurrent finalizer failed".into(),
            })
        } else {
            Ok(TurnFinalizationReport::complete())
        }
    }
}

#[derive(Debug)]
struct PanickingFinalizer;

#[async_trait]
impl TurnFinalizer for PanickingFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        panic!("test finalizer panic")
    }
}

#[async_trait]
impl TurnFinalizer for RecordingFinalizer {
    async fn finalize(
        &self,
        _context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        self.calls.lock().unwrap().push(self.name);
        if self.fail {
            return Err(TurnFinalizationError::Failed {
                code: "test.failed".into(),
                message: "test finalizer failed".into(),
            });
        }
        Ok(TurnFinalizationReport::complete())
    }
}

fn client_turn_id() -> TurnId {
    static NEXT_CLIENT_TURN: AtomicUsize = AtomicUsize::new(1);
    TurnId::new(format!(
        "caller-turn-{}",
        NEXT_CLIENT_TURN.fetch_add(1, Ordering::Relaxed)
    ))
    .unwrap()
}

#[derive(Debug)]
struct EmptyTools;

#[derive(Debug)]
struct DropOwner(Arc<AtomicUsize>);

impl Drop for DropOwner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl ToolRuntime for EmptyTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    fn prepare(
        &self,
        _invocation_id: &str,
        _call: ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }

    fn query(
        &self,
        _identity: &ToolResultIdentity,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }

    async fn wait(
        &self,
        _identity: &ToolResultIdentity,
        _cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }

    fn commit(&self, _identity: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        Err(ToolError::Execution("empty test Tool catalog".into()))
    }
}

#[derive(Debug)]
struct TestComposition;

fn test_pin(preset_id: &AgentPresetId) -> AgentCompositionPin {
    test_pin_with_digest(preset_id, 'a')
}

fn test_pin_with_digest(preset_id: &AgentPresetId, digit: char) -> AgentCompositionPin {
    AgentCompositionPin::new(
        preset_id.clone(),
        digit.to_string().repeat(64),
        Arc::new(EmptyTools),
        Arc::new(()),
    )
    .unwrap()
}

#[derive(Debug)]
struct MutableComposition {
    digest_digit: Mutex<char>,
    unavailable: AtomicBool,
    calls: AtomicUsize,
}

#[derive(Debug)]
struct DropTrackingComposition {
    calls: AtomicUsize,
    drops: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct UnboundedDiagnosticComposition;

#[async_trait]
impl AgentComposition for UnboundedDiagnosticComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        Err(AgentCompositionError::Unavailable {
            preset_id: preset_id.clone(),
            reason: "界".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES),
        })
    }
}

#[async_trait]
impl AgentComposition for DropTrackingComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        AgentCompositionPin::new(
            preset_id.clone(),
            "a".repeat(64),
            Arc::new(EmptyTools),
            Arc::new(DropOwner(Arc::clone(&self.drops))),
        )
    }
}

impl MutableComposition {
    fn new(digest_digit: char) -> Self {
        Self {
            digest_digit: Mutex::new(digest_digit),
            unavailable: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }

    fn select_digest(&self, digest_digit: char) {
        *self.digest_digit.lock().unwrap() = digest_digit;
    }

    fn set_unavailable(&self) {
        self.unavailable.store(true, Ordering::Release);
    }
}

#[async_trait]
impl AgentComposition for MutableComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.unavailable.load(Ordering::Acquire) {
            return Err(AgentCompositionError::Unavailable {
                preset_id: preset_id.clone(),
                reason: "test preset source is unavailable".into(),
            });
        }
        Ok(test_pin_with_digest(
            preset_id,
            *self.digest_digit.lock().unwrap(),
        ))
    }
}

#[async_trait]
impl AgentComposition for TestComposition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        Ok(test_pin(preset_id))
    }
}

fn composition() -> Arc<dyn AgentComposition> {
    Arc::new(TestComposition)
}

#[derive(Clone, Debug)]
struct TestCompositionFactory;

#[async_trait]
impl PluginFactory for TestCompositionFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<AgentCompositionContract>(composition())?;
        plan.defer(
            "withdraw test Agent composition",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

fn fresh(header: SessionHeader) -> SubmitSession {
    let pin = test_pin(header.agent_preset_id());
    SubmitSession::Fresh(PreparedFreshSession::new(header, pin).unwrap())
}

async fn resume(kernel: &SessionKernel, session_id: SessionId) -> SubmitSession {
    SubmitSession::Resume(kernel.prepare_resume(&session_id).await.unwrap())
}

fn profile() -> FrozenAgentSettings {
    FrozenAgentSettings::new(
        "default",
        "system",
        ModelRef::new("deployment", "model").unwrap(),
        SandboxMode::WorkspaceWrite,
        false,
    )
    .unwrap()
}

fn header(session_id: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session_id).unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        profile(),
    )
    .unwrap()
}

fn snapshot() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        call_id: "call-1".into(),
        deployment_id: "deployment".into(),
        provider_family: "test".into(),
        capability: AiCapability::Language,
        model: "model".into(),
        protocol: "test".into(),
        transport: "memory".into(),
        endpoint_fingerprint: "endpoint".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "a".repeat(64),
    }
}

fn accepted_fact(seq: u64, turn_id: &TurnId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::TurnAccepted {
            turn_id: turn_id.clone(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}

fn model_intent_fact(seq: u64, turn_id: &TurnId, effect_id: &EffectId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::ModelIntent {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
            snapshot: snapshot(),
        },
    )
    .unwrap()
}

fn model_started_fact(seq: u64, turn_id: &TurnId, effect_id: &EffectId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::ModelStarted {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
        },
    )
    .unwrap()
}

fn model_finished_fact(seq: u64, turn_id: &TurnId, effect_id: &EffectId) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::ModelEvent {
            turn_id: turn_id.clone(),
            effect_id: effect_id.clone(),
            event: LanguageEvent::Finished {
                reason: rsi_ai_protocol::FinishReason::Stop,
                replay: None,
            },
        },
    )
    .unwrap()
}

fn budget_fact(
    seq: u64,
    turn_id: &TurnId,
    dimension: BudgetDimension,
    consumed: u64,
    limit: u64,
) -> SessionFact {
    SessionFact::new(
        seq,
        seq,
        SessionFactBody::BudgetExhausted {
            turn_id: turn_id.clone(),
            dimension,
            consumed,
            limit,
        },
    )
    .unwrap()
}

async fn kernel(store: Arc<MemoryStore>) -> SessionKernel {
    let store: Arc<dyn SessionStore> = store;
    SessionKernel::recover_with_clock(store, composition(), Arc::new(FixedClock))
        .await
        .unwrap()
}

#[derive(Debug)]
struct QueuedWorkspaceContext {
    snapshots: Mutex<VecDeque<WorkspaceContextSnapshot>>,
    calls: AtomicUsize,
}

#[async_trait]
impl WorkspaceContext for QueuedWorkspaceContext {
    async fn snapshot(
        &self,
        _header: &SessionHeader,
        _messages: &[&AgentMessage],
    ) -> std::result::Result<WorkspaceContextSnapshot, WorkspaceContextError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.snapshots
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| WorkspaceContextError::Failed("snapshot queue exhausted".into()))
    }
}

async fn append_terminal_history(store: &MemoryStore, session_id: &str, turns: usize) {
    let session_id = SessionId::new(session_id).unwrap();
    let mut facts = Vec::with_capacity(turns * 2);
    for index in 0..turns {
        let turn_id = TurnId::new(format!("turn-history-{index}")).unwrap();
        let accepted_seq = u64::try_from(index * 2 + 1).unwrap();
        facts.push(
            SessionFact::new(
                accepted_seq,
                1,
                SessionFactBody::TurnAccepted {
                    turn_id: turn_id.clone(),
                    text: "done".into(),
                    model: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        facts.push(
            SessionFact::new(
                accepted_seq + 1,
                1,
                SessionFactBody::TurnTerminal {
                    turn_id,
                    outcome: TurnOutcome::Completed,
                },
            )
            .unwrap(),
        );
    }
    let mut expected_seq = 0;
    for (batch_index, batch_facts) in facts.chunks(512).enumerate() {
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq,
                header: (batch_index == 0).then(|| header(session_id.as_str())),
                facts: batch_facts.to_vec(),
            })
            .await
            .unwrap();
        expected_seq = batch_facts.last().unwrap().seq();
    }
}

async fn submit(
    kernel: &SessionKernel,
    session_id: &str,
    text: &str,
) -> rsi_agent_turn_protocol::SubmittedTurn {
    kernel
        .submit(SubmitTurn {
            turn_id: client_turn_id(),
            session: fresh(header(session_id)),
            text: text.into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap()
}

fn mailbox_message(message_id: &str) -> AgentMessage {
    AgentMessage {
        message_id: MessageId::new(message_id).unwrap(),
        source: AgentMessageSource::Human,
        content: vec![AgentMessageContent::Text {
            text: "hello from mailbox".into(),
        }],
        options: MessageOptions::default(),
    }
}

#[path = "kernel/agent_lifecycle.rs"]
mod agent_lifecycle;
#[path = "kernel/capacity_and_observation.rs"]
mod capacity_and_observation;
#[path = "kernel/fork_and_scheduler.rs"]
mod fork_and_scheduler;
#[path = "kernel/recovery_and_finalization.rs"]
mod recovery_and_finalization;
#[path = "kernel/settlement.rs"]
mod settlement;
#[path = "kernel/submission.rs"]
mod submission;
#[path = "kernel/workspace_and_mailbox.rs"]
mod workspace_and_mailbox;

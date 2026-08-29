use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_agent_kernel::{Clock, IdSource, KernelFactory, MAXIMUM_ACTIVE_SESSIONS, SessionKernel};
use rsi_agent_session_protocol::{
    EffectId, EffectKind, FrozenAgentProfile, MAXIMUM_AGENT_DIAGNOSTIC_BYTES,
    MAXIMUM_FACTS_PER_READ, MAXIMUM_TURN_TEXT_BYTES, SessionFact, SessionFactBody, SessionHeader,
    SessionId, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::{
    AppendBatch, AppendCommit, CasObjectRef, SessionStore, StoreFactPage, StoreOpenTurnPage,
    StoreSessionPage, StoreTurnFactPage,
};
use rsi_agent_testkit::{MemoryStore, MemoryStoreFactory};
use rsi_agent_turn_protocol::{
    SubmitSession, SubmitTurn, TurnError, TurnExecution, TurnFinalizationError, TurnFinalizer,
    TurnService, TurnUpdate,
};
use rsi_ai_protocol::{
    AiCapability, ContentDelta, ContentStart, LanguageEvent, MAX_LANGUAGE_OUTPUT_BYTES, ModelRef,
    PreparedCallSnapshot, RetryPolicy,
};
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use rsi_sandbox::SandboxMode;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        42
    }
}

#[derive(Debug, Default)]
struct SequenceIds(Mutex<u64>);

#[derive(Debug)]
struct FactReadRaceStore {
    inner: Arc<MemoryStore>,
    pause_read: AtomicBool,
    read_attempts: AtomicUsize,
    read_captured: Notify,
    release_read: Notify,
    pause_open_turn_read: AtomicBool,
    open_turn_read_attempts: AtomicUsize,
    open_turn_read_captured: Notify,
    release_open_turn_read: Notify,
    append_attempts: AtomicUsize,
    pause_append_at: AtomicUsize,
    append_blocked: Notify,
    release_append: Notify,
}

impl FactReadRaceStore {
    fn new(inner: Arc<MemoryStore>) -> Self {
        Self {
            inner,
            pause_read: AtomicBool::new(false),
            read_attempts: AtomicUsize::new(0),
            read_captured: Notify::new(),
            release_read: Notify::new(),
            pause_open_turn_read: AtomicBool::new(false),
            open_turn_read_attempts: AtomicUsize::new(0),
            open_turn_read_captured: Notify::new(),
            release_open_turn_read: Notify::new(),
            append_attempts: AtomicUsize::new(0),
            pause_append_at: AtomicUsize::new(0),
            append_blocked: Notify::new(),
            release_append: Notify::new(),
        }
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

    fn pause_second_following_append(&self) {
        let attempt = self.append_attempts.load(Ordering::Acquire) + 2;
        self.pause_append_at.store(attempt, Ordering::Release);
    }

    async fn wait_until_append_is_blocked(&self) {
        self.append_blocked.notified().await;
    }

    fn release_blocked_append(&self) {
        self.release_append.notify_one();
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
        self.inner.append(batch).await
    }

    async fn header(
        &self,
        session_id: &SessionId,
    ) -> rsi_agent_store_protocol::Result<SessionHeader> {
        self.inner.header(session_id).await
    }

    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_store_protocol::Result<StoreFactPage> {
        self.read_attempts.fetch_add(1, Ordering::AcqRel);
        let page = self.inner.read_facts(session_id, after_seq, limit).await?;
        if self.pause_read.swap(false, Ordering::AcqRel) {
            self.read_captured.notify_one();
            self.release_read.notified().await;
        }
        Ok(page)
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

#[async_trait]
impl TurnFinalizer for RecordingFinalizer {
    async fn finalize(
        &self,
        _session_id: &SessionId,
        _turn_id: &TurnId,
    ) -> rsi_agent_turn_protocol::FinalizationResult<()> {
        self.calls.lock().unwrap().push(self.name);
        if self.fail {
            return Err(TurnFinalizationError::Failed {
                code: "test.failed".into(),
                message: "test finalizer failed".into(),
            });
        }
        Ok(())
    }
}

impl IdSource for SequenceIds {
    fn next_id(&self, prefix: &str) -> rsi_agent_kernel::Result<String> {
        let mut next = self.0.lock().unwrap();
        *next += 1;
        Ok(format!("{prefix}-{next}"))
    }
}

fn profile() -> FrozenAgentProfile {
    FrozenAgentProfile::new(
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

async fn kernel(store: Arc<MemoryStore>) -> SessionKernel {
    let store: Arc<dyn SessionStore> = store;
    SessionKernel::recover_with_sources(
        store,
        Arc::new(FixedClock),
        Arc::new(SequenceIds::default()),
    )
    .await
    .unwrap()
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
            session: SubmitSession::Fresh(header(session_id)),
            text: text.into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn fresh_session_is_live_immediately_and_lazy_until_the_200ms_flush() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-lazy", "hello").await;
    let session = submitted.session_id.clone();
    assert!(store.header(&session).await.is_err());

    let mut observation = kernel.observe(&session, 0).await.unwrap();
    assert!(matches!(
        observation.next().await.unwrap().unwrap(),
        TurnUpdate::Fact { durable_seq: 0, .. }
    ));
    tokio::time::advance(std::time::Duration::from_millis(199)).await;
    tokio::task::yield_now().await;
    assert!(store.header(&session).await.is_err());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        store.header(&session).await.unwrap(),
        header("session-lazy")
    );
    assert_eq!(
        store.read_facts(&session, 0, 8).await.unwrap().durable_seq,
        1
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn explicit_effect_flush_waits_through_transient_failure_without_reordering() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-retry", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-1").unwrap();
    let facts = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect,
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap();
    store.fail_next_appends(1);
    let through = facts.last().unwrap().seq();
    let flush = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.flush(&claim, through).await }
    });
    tokio::task::yield_now().await;
    assert!(!flush.is_finished());
    assert!(store.header(&submitted.session_id).await.is_err());
    tokio::time::advance(std::time::Duration::from_millis(199)).await;
    tokio::task::yield_now().await;
    assert!(!flush.is_finished());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(flush.await.unwrap().unwrap(), through);
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert_eq!(stored.facts.len(), 2);
    assert!(matches!(
        stored.facts[0].body(),
        SessionFactBody::TurnAccepted { .. }
    ));
    assert!(matches!(
        stored.facts[1].body(),
        SessionFactBody::ModelIntent { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn effect_start_requires_its_intent_to_be_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-effect-fence", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-effect-fence").unwrap();

    let error = kernel
        .publish(
            &claim,
            vec![
                SessionFactBody::ModelIntent {
                    turn_id: submitted.turn_id.clone(),
                    effect_id: effect.clone(),
                    snapshot: snapshot(),
                },
                SessionFactBody::ModelStarted {
                    turn_id: submitted.turn_id.clone(),
                    effect_id: effect.clone(),
                },
            ],
        )
        .await
        .expect_err("an effect start cannot share the undurable intent publication");
    assert!(matches!(error, TurnError::Invalid(_)));

    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap();
    let through = intent.last().unwrap().seq();
    kernel.flush(&claim, through).await.unwrap();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: submitted.turn_id,
                effect_id: effect,
            }],
        )
        .await
        .unwrap();

    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancellation_single_assigns_cancelled_even_if_executor_reports_completed() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    assert!(
        kernel
            .cancel(
                &submitted.session_id,
                &submitted.turn_id,
                Some("stop".into())
            )
            .await
            .unwrap()
            .accepted
    );
    assert!(cancellation.is_cancelled());
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Cancelled,
            ..
        }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn claim_horizon_hides_later_accepted_turns_but_admits_claimed_turn_facts() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-horizon", "FIRST_PRIVATE_PROMPT").await;
    let later = kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(first.session_id.clone()),
            text: "LATER_PRIVATE_PROMPT".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id, first.turn_id);

    let initial = kernel.read_facts(&claim, 0, 8).await.unwrap();
    assert_eq!(initial.through_seq, later.accepted_seq);
    assert_eq!(initial.facts.len(), 1);
    assert!(matches!(
        initial.facts[0].body(),
        SessionFactBody::TurnAccepted { text, .. } if text == "FIRST_PRIVATE_PROMPT"
    ));

    let effect_id = EffectId::new("effect-horizon").unwrap();
    let published = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: first.turn_id.clone(),
                effect_id: effect_id.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap();
    let incremental = kernel
        .read_facts(&claim, initial.through_seq, 8)
        .await
        .unwrap();
    assert_eq!(incremental.facts, published);
    assert!(matches!(
        incremental.facts[0].body(),
        SessionFactBody::ModelIntent { effect_id: current, .. } if current == &effect_id
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // The deterministic interleaving keeps every race barrier visible.
async fn claim_fact_read_never_skips_a_prefix_committed_during_store_io() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel = SessionKernel::recover_with_sources(
        store.clone(),
        Arc::new(FixedClock),
        Arc::new(SequenceIds::default()),
    )
    .await
    .unwrap();
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-read-race", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("model-read-race").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap();
    kernel
        .flush(&claim, intent.last().unwrap().seq())
        .await
        .unwrap();
    let started = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: submitted.turn_id.clone(),
                effect_id: effect.clone(),
            }],
        )
        .await
        .unwrap();
    kernel
        .flush(&claim, started.last().unwrap().seq())
        .await
        .unwrap();
    worker.abort();
    let _ = worker.await;

    let mut first_batch = Vec::with_capacity(MAXIMUM_FACTS_PER_READ);
    first_batch.push(SessionFactBody::ModelEvent {
        turn_id: submitted.turn_id.clone(),
        effect_id: effect.clone(),
        event: LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
    });
    first_batch.extend(
        (1..MAXIMUM_FACTS_PER_READ).map(|_| SessionFactBody::ModelEvent {
            turn_id: submitted.turn_id.clone(),
            effect_id: effect.clone(),
            event: LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("x".into()),
            },
        }),
    );
    kernel.publish(&claim, first_batch).await.unwrap();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelEvent {
                turn_id: submitted.turn_id,
                effect_id: effect,
                event: LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("tail".into()),
                },
            }],
        )
        .await
        .unwrap();

    store.pause_next_read();
    let read = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.read_facts(&claim, 0, MAXIMUM_FACTS_PER_READ).await }
    });
    store.wait_until_read_is_captured().await;
    store.pause_second_following_append();
    let worker = kernel.start_write_behind();
    store.wait_until_append_is_blocked().await;
    store.release_captured_read();

    let page = read.await.unwrap().unwrap();
    assert_eq!(page.through_seq, 3);
    assert!(
        page.facts
            .windows(2)
            .all(|pair| pair[1].seq() == pair[0].seq() + 1),
        "a Store prefix committed during the read must be returned on a later page, not skipped: {:?}",
        page.facts.iter().map(SessionFact::seq).collect::<Vec<_>>()
    );
    let committed = kernel
        .read_facts(&claim, page.through_seq, MAXIMUM_FACTS_PER_READ)
        .await
        .unwrap();
    assert_eq!(committed.facts.first().map(SessionFact::seq), Some(4));
    assert_eq!(committed.facts.last().map(SessionFact::seq), Some(515));
    assert_eq!(committed.through_seq, 515);

    store.release_blocked_append();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn executor_cannot_classify_cancellation_without_a_durable_request() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-unrequested-cancel", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Cancelled,
            }],
        )
        .await
        .unwrap();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();

    assert!(matches!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Failed { code, .. }) if code == "executor.unrequested_cancellation"
    ));
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Failed { code, .. },
            ..
        } if code == "executor.unrequested_cancellation"
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn terminal_outcome_and_fact_are_hidden_until_their_prefix_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let submitted = submit(&kernel, "session-terminal-fence", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let terminal = kernel
        .publish(
            &claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: submitted.turn_id.clone(),
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        None
    );
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), observation.next())
            .await
            .is_err(),
        "a speculative terminal Fact must not enter observation"
    );

    let worker = kernel.start_write_behind();
    kernel
        .flush(&claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        kernel
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    let mut observed_terminal = false;
    while let Some(update) = observation.next().await {
        if matches!(
            update.unwrap(),
            TurnUpdate::Fact { fact, durable_seq }
                if fact.seq() == terminal[0].seq() && durable_seq >= fact.seq()
        ) {
            observed_terminal = true;
            break;
        }
    }
    assert!(observed_terminal);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancellation_does_not_fire_before_its_fact_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-durable", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    store.fail_next_appends(1);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move {
            kernel
                .cancel(&session_id, &turn_id, Some("stop".into()))
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!cancellation.is_cancelled());
    assert!(!cancelling.is_finished());

    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    assert!(cancelling.await.unwrap().unwrap().accepted);
    assert!(cancellation.is_cancelled());
    let stored = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
    assert!(matches!(
        stored.facts.last().unwrap().body(),
        SessionFactBody::CancelRequested { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn durable_cancellation_fires_even_after_the_requesting_future_detaches() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-detached", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let cancellation = kernel.cancellation(&claim).unwrap();
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    store.fail_next_appends(1);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });
    let update = observation.next().await.unwrap().unwrap();
    assert!(matches!(
        update,
        TurnUpdate::Fact { fact, .. }
            if matches!(fact.body(), SessionFactBody::CancelRequested { .. })
    ));
    cancelling.abort();
    let _ = cancelling.await;
    assert!(!cancellation.is_cancelled());

    tokio::time::advance(std::time::Duration::from_millis(400)).await;
    tokio::task::yield_now().await;
    assert!(
        cancellation.is_cancelled(),
        "durable commit, not request-future ownership, must fire the token"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn persistent_store_failure_eventually_latches_a_flush_error() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let submitted = submit(&kernel, "session-cancel-persistent-io", "hello").await;
    let mut observation = kernel
        .observe(&submitted.session_id, submitted.accepted_seq)
        .await
        .unwrap();
    store.fail_next_appends(usize::MAX);
    let cancelling = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = submitted.session_id.clone();
        let turn_id = submitted.turn_id.clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });

    for _ in 0..16 {
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        if cancelling.is_finished() {
            break;
        }
    }
    assert!(
        cancelling.is_finished(),
        "persistent I/O failure must not leave a public cancel future pending forever"
    );
    assert!(matches!(
        cancelling.await.unwrap(),
        Err(TurnError::Flush(_))
    ));
    assert!(matches!(
        observation.next().await,
        Some(Ok(TurnUpdate::Fact { fact, .. }))
            if matches!(fact.body(), SessionFactBody::CancelRequested { .. })
    ));
    let terminal = tokio::time::timeout(std::time::Duration::from_millis(1), observation.next())
        .await
        .expect("a latched flush error must terminate the attached observation");
    assert!(matches!(terminal, Some(Err(TurnError::Flush(_)))));
    assert!(matches!(
        kernel
            .submit(SubmitTurn {
                session: SubmitSession::Resume(submitted.session_id.clone()),
                text: "must not wedge behind the permanent failure".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Flush(_))
    ));
    assert!(kernel.shutdown(worker).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn failed_cancellation_admission_can_be_retried_after_capacity_recovers() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-cancel-full", "hello").await;
    let _lease = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("effect-fill").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                turn_id: first.turn_id.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap();
    kernel
        .flush(&claim, intent.last().unwrap().seq())
        .await
        .unwrap();
    kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelStarted {
                turn_id: first.turn_id.clone(),
                effect_id: effect.clone(),
            }],
        )
        .await
        .unwrap();
    let mut chunk = MAX_LANGUAGE_OUTPUT_BYTES;
    while chunk > 0 {
        let result = kernel
            .publish(
                &claim,
                vec![SessionFactBody::ModelEvent {
                    turn_id: first.turn_id.clone(),
                    effect_id: effect.clone(),
                    event: LanguageEvent::ContentDelta {
                        index: 0,
                        delta: ContentDelta::Text("x".repeat(chunk)),
                    },
                }],
            )
            .await;
        match result {
            Ok(_) => {}
            Err(TurnError::Flush(_)) => chunk /= 2,
            Err(error) => panic!("unexpected fill failure: {error}"),
        }
    }

    assert!(
        kernel
            .cancel(
                &first.session_id,
                &first.turn_id,
                Some("x".repeat(MAXIMUM_AGENT_DIAGNOSTIC_BYTES)),
            )
            .await
            .is_err(),
        "the full speculative suffix must reject the cancellation Fact"
    );

    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    let retry = kernel
        .cancel(&first.session_id, &first.turn_id, None)
        .await
        .unwrap();
    assert!(
        retry.accepted,
        "failed admission must not consume cancellation"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn shutdown_timeout_stops_the_worker_and_releases_its_store_owner() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    store.fail_next_appends(usize::MAX);
    let _submitted = submit(&kernel, "session-shutdown-failure", "hello").await;

    let shutdown = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.shutdown(worker).await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert!(shutdown.await.unwrap().is_err());
    drop(kernel);
    assert_eq!(
        Arc::strong_count(&store),
        1,
        "failed shutdown must not leave the Store owned by a detached worker"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_snapshots_flush_waiters_before_terminal_sessions_can_be_evicted() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-shutdown-a", "first").await;
    let second = submit(&kernel, "session-shutdown-b", "second").await;
    let _lease = kernel.register("executor".into()).unwrap();

    for submitted in [&first, &second] {
        let claim = kernel
            .claim("executor", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.turn_id, submitted.turn_id);
        kernel
            .publish(
                &claim,
                vec![SessionFactBody::TurnTerminal {
                    turn_id: submitted.turn_id.clone(),
                    outcome: TurnOutcome::Completed,
                }],
            )
            .await
            .unwrap();
    }

    kernel
        .shutdown(worker)
        .await
        .expect("terminal eviction must not invalidate a later shutdown waiter");
    for submitted in [first, second] {
        let page = store.read_facts(&submitted.session_id, 0, 8).await.unwrap();
        assert_eq!(page.durable_seq, 2);
        assert!(matches!(
            page.facts.last().map(SessionFact::body),
            Some(SessionFactBody::TurnTerminal { .. })
        ));
    }
}

#[tokio::test]
async fn next_turn_is_not_claimable_until_the_previous_terminal_is_durable() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_write_behind();
    let first = submit(&kernel, "session-queue", "first").await;
    let second = kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(first.session_id.clone()),
            text: "second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let _one = kernel.register("one".into()).unwrap();
    let _two = kernel.register("two".into()).unwrap();
    let first_claim = kernel
        .claim("one", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.turn_id, first.turn_id);
    let terminal = kernel
        .publish(
            &first_claim,
            vec![SessionFactBody::TurnTerminal {
                turn_id: first.turn_id,
                outcome: TurnOutcome::Completed,
            }],
        )
        .await
        .unwrap();
    let waiting = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.claim("two", CancellationToken::new()).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    kernel
        .flush(&first_claim, terminal.last().unwrap().seq())
        .await
        .unwrap();
    assert_eq!(
        waiting.await.unwrap().unwrap().unwrap().turn_id,
        second.turn_id
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn rejected_turn_does_not_leave_control_state_when_the_pending_suffix_is_full() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let text = "x".repeat(MAXIMUM_TURN_TEXT_BYTES);
    let first = submit(&kernel, "session-full", &text).await;
    let mut next_id = 2_u64;
    loop {
        let result = kernel
            .submit(SubmitTurn {
                session: SubmitSession::Resume(first.session_id.clone()),
                text: text.clone(),
                model: None,
                sandbox: None,
            })
            .await;
        if result.is_err() {
            let rejected = TurnId::new(format!("turn-{next_id}")).unwrap();
            assert!(matches!(
                kernel.outcome(&first.session_id, &rejected).await,
                Err(TurnError::TurnNotFound { turn, .. }) if turn == rejected.to_string()
            ));
            break;
        }
        next_id += 1;
    }
}

#[tokio::test]
async fn live_session_working_set_has_an_exact_global_bound() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        submit(&kernel, &format!("session-bound-{index}"), "queued").await;
    }
    assert_eq!(
        kernel
            .submit(SubmitTurn {
                session: SubmitSession::Fresh(header("session-bound-overflow")),
                text: "overflow".into(),
                model: None,
                sandbox: None,
            })
            .await,
        Err(TurnError::Capacity)
    );
}

#[tokio::test]
async fn cancelling_evicted_terminal_turns_does_not_consume_live_session_capacity() {
    let store = Arc::new(MemoryStore::new());
    let mut terminal_turns = Vec::new();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("terminal-session-{index}")).unwrap();
        let turn_id = TurnId::new(format!("terminal-turn-{index}")).unwrap();
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: Some(header(session_id.as_str())),
                facts: vec![
                    SessionFact::new(
                        1,
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
                    SessionFact::new(
                        2,
                        2,
                        SessionFactBody::TurnTerminal {
                            turn_id: turn_id.clone(),
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ],
            })
            .await
            .unwrap();
        terminal_turns.push((session_id, turn_id));
    }
    let kernel = kernel(store).await;
    for (session_id, turn_id) in terminal_turns {
        assert!(
            kernel
                .cancel(&session_id, &turn_id, Some("late".into()))
                .await
                .unwrap()
                .already_terminal
        );
    }

    kernel
        .submit(SubmitTurn {
            session: SubmitSession::Fresh(header("capacity-remains-free")),
            text: "new".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn invalid_resumes_of_idle_durable_sessions_do_not_consume_live_capacity() {
    let store = Arc::new(MemoryStore::new());
    let mut terminal_sessions = Vec::new();
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("invalid-resume-session-{index}")).unwrap();
        let turn_id = TurnId::new(format!("invalid-resume-turn-{index}")).unwrap();
        store
            .append(AppendBatch {
                session_id: session_id.clone(),
                expected_seq: 0,
                header: Some(header(session_id.as_str())),
                facts: vec![
                    SessionFact::new(
                        1,
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
                    SessionFact::new(
                        2,
                        2,
                        SessionFactBody::TurnTerminal {
                            turn_id,
                            outcome: TurnOutcome::Completed,
                        },
                    )
                    .unwrap(),
                ],
            })
            .await
            .unwrap();
        terminal_sessions.push(session_id);
    }
    let kernel = kernel(store).await;
    let oversized = "x".repeat(MAXIMUM_TURN_TEXT_BYTES + 1);
    for session_id in terminal_sessions {
        assert!(matches!(
            kernel
                .submit(SubmitTurn {
                    session: SubmitSession::Resume(session_id),
                    text: oversized.clone(),
                    model: None,
                    sandbox: None,
                })
                .await,
            Err(TurnError::Invalid(_))
        ));
    }

    kernel
        .submit(SubmitTurn {
            session: SubmitSession::Fresh(header("capacity-after-invalid-resumes")),
            text: "new".into(),
            model: None,
            sandbox: None,
        })
        .await
        .expect("invalid resume input must not retain idle durable sessions");
}

#[tokio::test]
async fn historical_outcome_lookup_does_not_page_the_complete_session_log() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-indexed-outcome", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_sources(
        store_contract,
        Arc::new(FixedClock),
        Arc::new(SequenceIds::default()),
    )
    .await
    .unwrap();
    store.reset_read_attempts();

    assert_eq!(
        kernel
            .outcome(
                &SessionId::new("session-indexed-outcome").unwrap(),
                &TurnId::new("turn-history-299").unwrap(),
            )
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(
        store.read_attempts(),
        0,
        "an outcome lookup must use the Store's turn index, not full-log pages"
    );
}

#[tokio::test]
async fn recovery_skips_fact_pages_for_sessions_without_open_turns() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-recovery-index", 300).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();

    SessionKernel::recover_with_sources(
        store_contract,
        Arc::new(FixedClock),
        Arc::new(SequenceIds::default()),
    )
    .await
    .unwrap();

    assert_eq!(
        store.read_attempts(),
        0,
        "recovery must query the bounded open-turn index before decoding Fact bodies"
    );
}

#[tokio::test]
async fn concurrent_resumes_join_one_control_state_load() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "session-joined-load", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let store_contract: Arc<dyn SessionStore> = store.clone();
    let kernel = SessionKernel::recover_with_sources(
        store_contract,
        Arc::new(FixedClock),
        Arc::new(SequenceIds::default()),
    )
    .await
    .unwrap();
    store.reset_read_attempts();
    store.reset_open_turn_read_attempts();
    store.pause_next_open_turn_read();

    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    session: SubmitSession::Resume(SessionId::new("session-joined-load").unwrap()),
                    text: "first".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_open_turn_read_is_captured().await;
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    session: SubmitSession::Resume(SessionId::new("session-joined-load").unwrap()),
                    text: "second".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    store.release_captured_open_turn_read();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    assert_eq!(
        store.open_turn_read_attempts(),
        1,
        "concurrent resumes of one idle session must join one Store load"
    );
}

#[tokio::test]
async fn recovery_appends_interrupted_for_a_started_external_effect_and_never_requeues_it() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery").unwrap();
    let turn = TurnId::new("turn-recovery").unwrap();
    let effect = EffectId::new("effect-recovery").unwrap();
    let facts = vec![
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: turn.clone(),
                text: "hello".into(),
                model: None,
                sandbox: SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
        SessionFact::new(
            2,
            2,
            SessionFactBody::ModelIntent {
                turn_id: turn.clone(),
                effect_id: effect.clone(),
                snapshot: snapshot(),
            },
        )
        .unwrap(),
        SessionFact::new(
            3,
            3,
            SessionFactBody::ModelStarted {
                turn_id: turn.clone(),
                effect_id: effect,
            },
        )
        .unwrap(),
    ];
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-recovery")),
            facts,
        })
        .await
        .unwrap();
    let kernel = kernel(store.clone()).await;
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Interrupted {
            effect: Some(EffectKind::Model),
            reason: "Kernel recovery found a turn without a durable terminal Fact".into(),
        })
    );
    let repaired = store.read_facts(&session, 3, 8).await.unwrap();
    assert_eq!(repaired.facts.len(), 1);
    assert!(matches!(
        repaired.facts[0].body(),
        SessionFactBody::TurnTerminal {
            outcome: TurnOutcome::Interrupted {
                effect: Some(EffectKind::Model),
                ..
            },
            ..
        }
    ));
    let _lease = kernel.register("executor".into()).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(
        kernel
            .claim("executor", cancellation)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn recovery_preserves_a_durable_cancellation_classification() {
    let store = Arc::new(MemoryStore::new());
    let session = SessionId::new("session-recovery-cancelled").unwrap();
    let turn = TurnId::new("turn-recovery-cancelled").unwrap();
    store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(header("session-recovery-cancelled")),
            facts: vec![
                SessionFact::new(
                    1,
                    1,
                    SessionFactBody::TurnAccepted {
                        turn_id: turn.clone(),
                        text: "hello".into(),
                        model: None,
                        sandbox: SandboxMode::WorkspaceWrite,
                        require_approval: false,
                    },
                )
                .unwrap(),
                SessionFact::new(
                    2,
                    2,
                    SessionFactBody::CancelRequested {
                        turn_id: turn.clone(),
                        reason: Some("stop".into()),
                    },
                )
                .unwrap(),
            ],
        })
        .await
        .unwrap();

    let kernel = kernel(store.clone()).await;
    assert_eq!(
        kernel.outcome(&session, &turn).await.unwrap(),
        Some(TurnOutcome::Cancelled)
    );
    let repaired = store.read_facts(&session, 2, 8).await.unwrap();
    assert!(matches!(
        repaired.facts.as_slice(),
        [fact]
            if matches!(
                fact.body(),
                SessionFactBody::TurnTerminal {
                    outcome: TurnOutcome::Cancelled,
                    ..
                }
            )
    ));
}

#[tokio::test]
async fn ordinary_factory_waits_for_store_and_withdraws_all_turn_contracts() {
    let runtime = Runtime::default();
    let kernel_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.agent.kernel",
                "kernel",
                UpdateMode::Replayable,
                Arc::new(KernelFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    let store = Arc::new(MemoryStore::new());
    let store_fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.agent.store.memory",
                "store",
                UpdateMode::Replayable,
                Arc::new(MemoryStoreFactory::new(store)),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_some()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .is_some()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnFinalizationContract>()
            .is_some()
    );
    assert!(kernel_fiber.dispose().await.is_clean());
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnServiceContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnExecutionContract>()
            .is_none()
    );
    assert!(
        runtime
            .root()
            .lookup_local::<rsi_agent_turn_protocol::TurnFinalizationContract>()
            .is_none()
    );
    assert!(store_fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn finalizers_are_effect_owned_ordered_and_fail_fast() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let make = |name, fail| {
        Arc::new(RecordingFinalizer {
            name,
            calls: calls.clone(),
            fail,
        }) as Arc<dyn TurnFinalizer>
    };
    let first = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "first".into(),
        make("first", false),
    )
    .unwrap();
    let failing = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "failing".into(),
        make("failing", true),
    )
    .unwrap();
    let _never = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "never".into(),
        make("never", false),
    )
    .unwrap();
    assert!(matches!(
        rsi_agent_turn_protocol::TurnFinalization::register(
            &kernel,
            "first".into(),
            make("duplicate", false)
        ),
        Err(TurnFinalizationError::Invalid(_))
    ));

    let session = SessionId::new("session-finalizers").unwrap();
    let turn = TurnId::new("turn-finalizers").unwrap();
    assert_eq!(
        rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &session, &turn).await,
        Err(TurnFinalizationError::Failed {
            code: "test.failed".into(),
            message: "test finalizer failed".into(),
        })
    );
    assert_eq!(*calls.lock().unwrap(), vec!["first", "failing"]);

    calls.lock().unwrap().clear();
    drop(failing);
    let _replacement = rsi_agent_turn_protocol::TurnFinalization::register(
        &kernel,
        "failing".into(),
        make("replacement", false),
    )
    .unwrap();
    rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &session, &turn)
        .await
        .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["first", "never", "replacement"]
    );

    calls.lock().unwrap().clear();
    drop(first);
    rsi_agent_turn_protocol::TurnFinalization::finalize(&kernel, &session, &turn)
        .await
        .unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["never", "replacement"]);
}

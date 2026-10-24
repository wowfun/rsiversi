use super::{EvidenceReader, Key};
use rsi_agent_session_protocol::{
    EvidencePart, EvidenceSection, RequestEvidence, SessionFact, SessionFactBody, SessionHeader,
    SessionId, TurnId,
};
use rsi_agent_store_protocol::*;
use rsi_execution::ExecutionLocations;
use rsi_session_protocol::{EvidencePageContent, EvidenceRead, SessionError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Store {
    facts: Vec<SessionFact>,
    calls: AtomicUsize,
    gate: CancellationToken,
    entered: tokio::sync::Semaphore,
    fail: AtomicBool,
}
impl Store {
    fn new(text: String, blocked: bool) -> Arc<Self> {
        let inline = EvidencePart::inline(text);
        let reference = EvidencePart::Reference {
            seq: 1,
            section: EvidenceSection::System,
            sha256: inline.sha256().into(),
            bytes: u32::try_from(inline.bytes()).unwrap(),
        };
        let fact = |seq, system| {
            SessionFact::new(
                seq,
                seq,
                SessionFactBody::ModelIntent {
                    turn_id: TurnId::new("turn").unwrap(),
                    effect_id: rsi_agent_session_protocol::EffectId::new(format!("effect-{seq}"))
                        .unwrap(),
                    purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                    price_quote: None,
                    snapshot: rsi_ai_protocol::PreparedCallSnapshot {
                        call_id: format!("call-{seq}"),
                        deployment_id: "fixture".into(),
                        provider_family: "fixture".into(),
                        capability: rsi_ai_protocol::AiCapability::Language,
                        model: "model".into(),
                        protocol: "fixture".into(),
                        transport: "local".into(),
                        endpoint_fingerprint: "fixture".into(),
                        config_generation: 1,
                        credential_source: None,
                        retry_policy: rsi_ai_protocol::RetryPolicy::default(),
                        request_sha256: "0".repeat(64),
                        language_settings: None,
                    },
                    evidence: RequestEvidence::Available {
                        configuration: EvidencePart::inline(String::new()),
                        system,
                        tools: EvidencePart::inline(String::new()),
                        manifest: vec![],
                    },
                },
            )
            .unwrap()
        };
        let gate = CancellationToken::new();
        if !blocked {
            gate.cancel();
        }
        Arc::new(Self {
            facts: vec![fact(1, inline), fact(2, reference)],
            calls: AtomicUsize::new(0),
            gate,
            entered: tokio::sync::Semaphore::new(0),
            fail: AtomicBool::new(false),
        })
    }
    async fn entered(&self, count: u32) {
        self.entered.acquire_many(count).await.unwrap().forget();
    }
}
fn key(session: &str, seq: u64) -> Key {
    Key(
        SessionId::new(session).unwrap(),
        seq,
        EvidenceSection::System,
    )
}
fn request(seq: u64, offset: u32) -> EvidenceRead {
    EvidenceRead {
        intent_seq: seq,
        section: EvidenceSection::System,
        offset,
        maximum_bytes: 256 * 1024,
    }
}

#[tokio::test]
async fn byte_admission_keeps_zero_weight_results() {
    let cache = Arc::new(EvidenceReader {
        budget: rsi_api_protocol::ByteBudget::new(
            rsi_agent_session_protocol::MAXIMUM_REQUEST_EVIDENCE_BYTES + 8,
        )
        .unwrap(),
        ..EvidenceReader::default()
    });
    let empty = Store::new(String::new(), false);
    cache
        .load(empty.clone(), key("empty", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    let full = Store::new("x".repeat(16), false);
    cache
        .load(full.clone(), key("full", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    cache
        .load(full, key("next", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    let calls = empty.calls.load(Ordering::SeqCst);
    cache
        .load(empty.clone(), key("empty", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    assert_eq!(empty.calls.load(Ordering::SeqCst), calls);
    assert!(
        !cache
            .state
            .lock()
            .unwrap()
            .entries
            .contains_key(&key("full", 1))
    );
    cache.stop().await.unwrap();
}

#[tokio::test]
async fn unavailable_results_obey_entry_capacity_and_completion_waiter_pins() {
    let mut store = Store::new(String::new(), true);
    let facts = &mut Arc::get_mut(&mut store).unwrap().facts;
    let mut body = facts.remove(0).into_body();
    let SessionFactBody::ModelIntent { evidence, .. } = &mut body else {
        unreachable!();
    };
    *evidence = RequestEvidence::Unavailable {
        reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
    };
    *facts = vec![SessionFact::new(1, 1, body).unwrap()];
    let cache = Arc::new(EvidenceReader::default());
    let first = tokio::spawn({
        let cache = cache.clone();
        let store = store.clone();
        async move {
            cache
                .load(store, key("pin-0", 1), CancellationToken::new(), ())
                .await
        }
    });
    store.entered(1).await;
    let receiver = {
        let state = cache.state.lock().unwrap();
        let super::Entry::Loading(receiver) = &state.entries[&key("pin-0", 1)].entry else {
            panic!("Store is still blocked");
        };
        receiver.clone()
    };
    store.gate.cancel();
    let mut pins = vec![first.await.unwrap().unwrap()];
    for i in 1..super::MAXIMUM_ENTRIES {
        let selected = cache
            .load(
                store.clone(),
                key(&format!("pin-{i}"), 1),
                CancellationToken::new(),
                (),
            )
            .await
            .unwrap();
        assert!(matches!(
            selected.page(&request(1, 0)).unwrap().content,
            EvidencePageContent::Unavailable { .. }
        ));
        pins.push(selected);
    }
    assert_eq!(cache.budget.used(), 0);
    assert!(matches!(
        cache
            .load(store.clone(), key("next", 1), CancellationToken::new(), ())
            .await,
        Err(SessionError::Capacity)
    ));
    assert_eq!(store.calls.load(Ordering::SeqCst), super::MAXIMUM_ENTRIES);
    // A completed watch receiver still owns its result after all page readers
    // release it. Admission must wait for that ownership too.
    drop(pins.remove(0));
    assert!(matches!(
        cache
            .load(store.clone(), key("next", 1), CancellationToken::new(), ())
            .await,
        Err(SessionError::Capacity)
    ));
    drop(receiver);
    cache
        .load(store.clone(), key("next", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    assert_eq!(
        store.calls.load(Ordering::SeqCst),
        super::MAXIMUM_ENTRIES + 1
    );
    assert!(
        !cache
            .state
            .lock()
            .unwrap()
            .entries
            .contains_key(&key("pin-0", 1))
    );
    drop(pins);
    cache.stop().await.unwrap();
}

#[tokio::test]
async fn rejected_cold_admission_preserves_idle_entries() {
    let cache = Arc::new(EvidenceReader {
        budget: rsi_api_protocol::ByteBudget::new(
            rsi_agent_session_protocol::MAXIMUM_REQUEST_EVIDENCE_BYTES + 16,
        )
        .unwrap(),
        ..EvidenceReader::default()
    });
    let small = Store::new("idle".into(), false);
    drop(
        cache
            .load(small.clone(), key("idle", 1), CancellationToken::new(), ())
            .await
            .unwrap(),
    );
    let large = Store::new("x".repeat(32), false);
    let pinned = cache
        .load(large, key("pinned", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    assert!(matches!(
        cache
            .load(
                small.clone(),
                key("rejected", 1),
                CancellationToken::new(),
                ()
            )
            .await,
        Err(SessionError::Capacity)
    ));
    assert!(
        cache
            .state
            .lock()
            .unwrap()
            .entries
            .contains_key(&key("idle", 1))
    );
    drop(
        cache
            .load(small.clone(), key("idle", 1), CancellationToken::new(), ())
            .await
            .unwrap(),
    );
    assert_eq!(small.calls.load(Ordering::SeqCst), 1);
    drop(pinned);
    cache.stop().await.unwrap();
}

#[tokio::test]
async fn cold_admission_reclaims_idle_bytes_against_the_actual_budget() {
    let cache = Arc::new(EvidenceReader {
        budget: rsi_api_protocol::ByteBudget::new(
            rsi_agent_session_protocol::MAXIMUM_REQUEST_EVIDENCE_BYTES,
        )
        .unwrap(),
        ..EvidenceReader::default()
    });
    let store = Store::new("small section".into(), false);
    drop(
        cache
            .load(store.clone(), key("first", 1), CancellationToken::new(), ())
            .await
            .unwrap(),
    );
    drop(
        cache
            .load(
                store.clone(),
                key("second", 1),
                CancellationToken::new(),
                (),
            )
            .await
            .unwrap(),
    );
    assert_eq!(store.calls.load(Ordering::SeqCst), 2);
    {
        let state = cache.state.lock().unwrap();
        assert!(!state.entries.contains_key(&key("first", 1)));
        assert!(state.entries.contains_key(&key("second", 1)));
    }
    cache.stop().await.unwrap();
    assert_eq!(cache.budget.used(), 0);
}

#[tokio::test]
async fn sixty_four_pages_reuse_only_the_selected_section_and_exact_reference() {
    let text = "证据".repeat((16 * 1024 * 1024 - 64) / 6);
    let store = Store::new(text.clone(), false);
    let cache = Arc::new(EvidenceReader::default());
    for (seq, expected_reads) in [(1, 1), (2, 3)] {
        let mut offset = 0;
        let mut joined = String::new();
        let mut pages = 0;
        loop {
            let page = cache
                .read(
                    store.clone(),
                    SessionId::new("session").unwrap(),
                    &request(seq, offset),
                    CancellationToken::new(),
                    (),
                )
                .await
                .unwrap();
            let EvidencePageContent::Available {
                sha256,
                start,
                text: part,
                more,
                total_bytes,
            } = page.content
            else {
                panic!("source")
            };
            assert_eq!(start, offset);
            assert_eq!(total_bytes as usize, text.len());
            let SessionFactBody::ModelIntent { evidence, .. } = store.facts[0].body() else {
                panic!("intent")
            };
            assert_eq!(
                sha256,
                evidence.part(EvidenceSection::System).unwrap().sha256()
            );
            offset += u32::try_from(part.len()).unwrap();
            joined.push_str(&part);
            pages += 1;
            if !more {
                break;
            }
        }
        assert_eq!(joined, text);
        assert_eq!(pages, 64);
        assert_eq!(store.calls.load(Ordering::SeqCst), expected_reads);
    }
    cache.stop().await.unwrap();
    assert_eq!(cache.budget.used(), 0);
}

#[derive(Clone)]
struct Guard(Arc<AtomicUsize>);

#[tokio::test]
async fn invalid_reference_binding_is_not_cached() {
    let mut store = Store::new("source".into(), false);
    let mut body = store.facts[1].body().clone();
    if let SessionFactBody::ModelIntent {
        evidence:
            RequestEvidence::Available {
                system: EvidencePart::Reference { sha256, .. },
                ..
            },
        ..
    } = &mut body
    {
        *sha256 = "0".repeat(64);
    } else {
        panic!("reference");
    }
    Arc::get_mut(&mut store).unwrap().facts[1] = SessionFact::new(2, 2, body).unwrap();
    let cache = Arc::new(EvidenceReader::default());
    for expected in [2, 4] {
        assert!(matches!(
            cache
                .load(
                    store.clone(),
                    key("session", 2),
                    CancellationToken::new(),
                    ()
                )
                .await,
            Err(SessionError::Invalid(_))
        ));
        assert_eq!(store.calls.load(Ordering::SeqCst), expected);
        assert_eq!(cache.budget.used(), 0);
    }
    cache
        .load(store, key("session", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    cache.stop().await.unwrap();
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn shared_load_outlives_waiters_and_shutdown_retains_actual_work() {
    let store = Store::new("source".into(), true);
    let cache = Arc::new(EvidenceReader::default());
    let drops = Arc::new(AtomicUsize::new(0));
    let first = tokio::spawn({
        let cache = cache.clone();
        let store = store.clone();
        let guard = Guard(drops.clone());
        async move {
            cache
                .load(store, key("one", 2), CancellationToken::new(), guard)
                .await
        }
    });
    store.entered(1).await;
    let second = tokio::spawn({
        let cache = cache.clone();
        let store = store.clone();
        async move {
            cache
                .load(store, key("one", 2), CancellationToken::new(), ())
                .await
        }
    });
    tokio::task::yield_now().await;
    first.abort();
    second.abort();
    let _ = first.await;
    let _ = second.await;
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(cache.budget.used(), 16 * 1024 * 1024);
    assert!(matches!(cache.stop().await, Err(SessionError::Backend(_))));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(cache.budget.used(), 16 * 1024 * 1024);
    assert!(cache.state.lock().unwrap().entries.is_empty());
    cache.close();
    for closed in [key("one", 2), key("new", 1)] {
        assert!(matches!(
            cache
                .load(store.clone(), closed, CancellationToken::new(), ())
                .await,
            Err(SessionError::ShuttingDown)
        ));
    }
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);
    store.gate.cancel();
    cache.tasks.wait().await;
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(cache.budget.used(), 0);
    assert_eq!(cache.workers.available_permits(), super::MAXIMUM_COLD_LOADS);
    assert_eq!(
        store.calls.load(Ordering::SeqCst),
        1,
        "closed loader must not resolve the reference"
    );
    assert!(cache.state.lock().unwrap().entries.is_empty());
    cache.stop().await.unwrap();
}

#[tokio::test]
async fn session_retirement_stops_followup_reads_and_late_cache_publication() {
    let store = Store::new("source".into(), true);
    let cache = Arc::new(EvidenceReader::default());
    let retiring = CancellationToken::new();
    let drops = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let cache = cache.clone();
        let store = store.clone();
        let retiring = retiring.clone();
        let guard = Guard(drops.clone());
        async move { cache.load(store, key("retiring", 2), retiring, guard).await }
    });
    store.entered(1).await;
    retiring.cancel();
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "dispatched Store work still owns the read"
    );
    store.gate.cancel();
    assert!(matches!(
        task.await.unwrap(),
        Err(SessionError::ShuttingDown)
    ));
    assert_eq!(
        store.calls.load(Ordering::SeqCst),
        1,
        "no follow-up original read"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(cache.state.lock().unwrap().entries.is_empty());
    assert_eq!(cache.budget.used(), 0);
    cache.stop().await.unwrap();
}

#[tokio::test]
async fn cold_loads_are_bounded_errors_retry_and_idle_entries_are_evicted() {
    let store = Store::new("source".into(), true);
    let cache = Arc::new(EvidenceReader::default());
    let mut callers = Vec::new();
    for i in 0..4 {
        callers.push(tokio::spawn({
            let cache = cache.clone();
            let store = store.clone();
            async move {
                cache
                    .load(
                        store,
                        key(&format!("session-{i}"), 1),
                        CancellationToken::new(),
                        (),
                    )
                    .await
            }
        }));
    }
    store.entered(4).await;
    assert!(matches!(
        cache
            .load(store.clone(), key("fifth", 1), CancellationToken::new(), ())
            .await,
        Err(SessionError::Capacity)
    ));
    assert_eq!(store.calls.load(Ordering::SeqCst), 4);
    store.gate.cancel();
    let mut pins = Vec::new();
    for caller in callers {
        pins.push(caller.await.unwrap().unwrap());
    }
    store.fail.store(true, Ordering::SeqCst);
    assert!(
        cache
            .load(store.clone(), key("retry", 1), CancellationToken::new(), ())
            .await
            .is_err()
    );
    assert!(
        cache
            .load(store.clone(), key("retry", 1), CancellationToken::new(), ())
            .await
            .is_ok()
    );
    for i in 0..65 {
        cache
            .load(
                store.clone(),
                key(&format!("idle-{i}"), 1),
                CancellationToken::new(),
                (),
            )
            .await
            .unwrap();
    }
    {
        let entries = cache.state.lock().unwrap();
        assert_eq!(entries.entries.len(), 64);
        assert!(!entries.entries.contains_key(&key("idle-0", 1)));
        assert!(
            entries.entries.contains_key(&key("session-0", 1)),
            "pinned entries survive LRU"
        );
    }
    drop(pins);
    cache.stop().await.unwrap();
    assert_eq!(cache.budget.used(), 0);
}

#[tokio::test]
async fn byte_capacity_is_reclaimed_only_from_idle_selected_sources() {
    let store = Store::new("x".repeat(16 * 1024 * 1024 - 64), false);
    let cache = Arc::new(EvidenceReader::default());
    let mut pins = Vec::new();
    for i in 0..4 {
        pins.push(
            cache
                .load(
                    store.clone(),
                    key(&format!("pinned-{i}"), 1),
                    CancellationToken::new(),
                    (),
                )
                .await
                .unwrap(),
        );
    }
    assert!(matches!(
        cache
            .load(store.clone(), key("next", 1), CancellationToken::new(), ())
            .await,
        Err(SessionError::Capacity)
    ));
    assert_eq!(store.calls.load(Ordering::SeqCst), 4);
    drop(pins.remove(0));
    cache
        .load(store.clone(), key("next", 1), CancellationToken::new(), ())
        .await
        .unwrap();
    assert_eq!(store.calls.load(Ordering::SeqCst), 5);
    assert!(
        !cache
            .state
            .lock()
            .unwrap()
            .entries
            .contains_key(&key("pinned-0", 1))
    );
    drop(pins);
    cache.stop().await.unwrap();
    assert_eq!(cache.budget.used(), 0);
}

#[async_trait::async_trait]
#[allow(unused_variables)]
impl SessionStore for Store {
    async fn session_activity_summaries(
        &self,
        sessions: &[SessionId],
    ) -> Result<Vec<Option<StoreActivityRow>>> {
        unreachable!()
    }
    async fn list_session_activity(
        &self,
        locations: &rsi_execution::ExecutionLocations,
        coordinates: Option<&rsi_agent_session_protocol::ExecutionCoordinates>,
        after: Option<&StoreActivityCursor>,
        limit: usize,
    ) -> Result<StoreActivityPage> {
        unreachable!()
    }
    async fn session_order_seed(
        &self,
        locations: &rsi_execution::ExecutionLocations,
        coordinates: Option<&rsi_agent_session_protocol::ExecutionCoordinates>,
    ) -> Result<StoreOrderSeed> {
        unreachable!()
    }
    async fn prepare_session(&self, session_id: &SessionId) -> Result<SessionValidationLease> {
        unreachable!()
    }
    async fn read_watermarks(&self, session_id: &SessionId) -> Result<StoreSessionWatermarks> {
        unreachable!()
    }
    async fn read_fact_suffix(
        &self,
        session_id: &SessionId,
        limit: usize,
        maximum_bytes: usize,
    ) -> Result<StoreFactSuffix> {
        unreachable!()
    }
    async fn append(&self, batch: AppendBatch) -> Result<AppendCommit> {
        unreachable!()
    }
    async fn validate_session(&self, session_id: &SessionId) -> Result<()> {
        unreachable!()
    }
    async fn header(&self, session_id: &SessionId) -> Result<SessionHeader> {
        unreachable!()
    }
    async fn read_facts_before(
        &self,
        session_id: &SessionId,
        exclusive_before_seq: u64,
        limit: usize,
    ) -> Result<StoreBackwardFactPage> {
        unreachable!()
    }
    async fn read_turn_facts(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreTurnFactPage> {
        unreachable!()
    }
    async fn read_turn_boundary(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> Result<StoreTurnBoundary> {
        unreachable!()
    }
    async fn list_open_turns(
        &self,
        session_id: &SessionId,
        after_accepted_seq: u64,
        limit: usize,
    ) -> Result<StoreOpenTurnPage> {
        unreachable!()
    }
    async fn list_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage> {
        unreachable!()
    }
    async fn list_recent_sessions(
        &self,
        locations: &ExecutionLocations,
        after: Option<&StoreRecentSessionCursor>,
        limit: usize,
    ) -> Result<StoreRecentSessionPage> {
        unreachable!()
    }
    async fn list_open_sessions(
        &self,
        after: Option<&SessionId>,
        limit: usize,
    ) -> Result<StoreSessionPage> {
        unreachable!()
    }
    async fn retain_cas(
        &self,
        object: &CasObjectRef,
    ) -> rsi_agent_store_protocol::Result<rsi_agent_store_protocol::PreparedCasObject> {
        let _ = object;
        Err(StoreError::Unsupported("CAS".into()))
    }
    async fn put_cas(
        &self,
        bytes: Arc<[u8]>,
    ) -> Result<rsi_agent_store_protocol::PreparedCasObject> {
        unreachable!()
    }
    async fn read_cas(
        &self,
        object: &CasObjectRef,
        admission: rsi_api_protocol::ByteAdmission,
    ) -> Result<rsi_api_protocol::RetainedBytes> {
        unreachable!()
    }
    async fn read_facts(
        &self,
        session_id: &SessionId,
        after_seq: u64,
        limit: usize,
    ) -> Result<StoreFactPage> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        self.gate.cancelled().await;
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(StoreError::NotFound("injected failure".into()));
        }
        Ok(StoreFactPage {
            after_seq,
            durable_seq: 2,
            facts: self
                .facts
                .iter()
                .filter(|fact| fact.seq() > after_seq)
                .take(limit)
                .cloned()
                .collect(),
        })
    }
}

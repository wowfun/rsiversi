use super::*;
use rsi_agent_composition_protocol::{
    ContributionError, SessionProjection, SessionProjectionContext,
};
use rsi_agent_session_protocol::{ProjectionCursor, ProjectionValue};
use rsi_agent_turn_protocol::SessionProjections;
use std::time::Duration;

#[derive(Debug)]
pub(super) struct PinGate {
    pub(super) entered: tokio::sync::Semaphore,
    pub(super) release: tokio::sync::Semaphore,
}
impl PinGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        })
    }
}

#[tokio::test]
async fn resident_manifest_peek_never_resolves_cold_or_replaces_a_retained_generation() {
    use rsi_agent_composition_protocol::{
        CompositionInstance, CompositionManifest, CompositionOrigin,
    };
    use rsi_agent_turn_protocol::ResidentComposition;
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let calls = fixture.composition.calls.load(Ordering::SeqCst);
    fixture.composition.reject.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .kernel
            .resident_activity()
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(matches!(
        fixture
            .kernel
            .resident_composition(&fixture.session_id)
            .unwrap(),
        ResidentComposition::NotResident
    ));
    assert_eq!(fixture.composition.calls.load(Ordering::SeqCst), calls);
    fixture.composition.reject.store(false, Ordering::SeqCst);
    let manifest = Arc::new(
        CompositionManifest::new(vec![CompositionInstance {
            instance: "captured-a".into(),
            plugin: "fixture.a".into(),
            enabled: true,
            origin: CompositionOrigin::Linked,
        }])
        .unwrap(),
    );
    let pin = fixture
        .composition
        .pin
        .read()
        .unwrap()
        .clone()
        .with_manifest(manifest.clone());
    *fixture.composition.pin.write().unwrap() = pin;
    let prepared = fixture
        .kernel
        .prepare_resume(&fixture.session_id)
        .await
        .unwrap();
    fixture
        .kernel
        .submit(SubmitTurn {
            reasoning_effort: None,
            session: SubmitSession::Resume(prepared),
            turn_id: TurnId::new("resident-manifest").unwrap(),
            text: "retain generation".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    fixture.composition.reject.store(true, Ordering::SeqCst);
    let calls = fixture.composition.calls.load(Ordering::SeqCst);
    let ResidentComposition::Resident {
        header,
        manifest: captured,
        source_digest,
    } = fixture
        .kernel
        .resident_composition(&fixture.session_id)
        .unwrap()
    else {
        panic!("resident evidence missing")
    };
    assert_eq!(header.session_id(), &fixture.session_id);
    assert_eq!(captured.unwrap(), manifest);
    assert_eq!(source_digest, "c".repeat(64));
    let activity = fixture.kernel.resident_activity().unwrap();
    assert_eq!(activity.entries.len(), 1);
    assert_eq!(activity.entries[0].session, fixture.session_id);
    assert!(activity.entries[0].running);
    assert!(!activity.has_more);
    assert_eq!(fixture.composition.calls.load(Ordering::SeqCst), calls);
    fixture.stop().await;
}

#[tokio::test]
async fn failed_cold_capture_yields_to_a_concurrently_published_resident_pin() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let prepared = fixture
        .kernel
        .prepare_resume(&fixture.session_id)
        .await
        .unwrap();
    let gate = PinGate::new();
    *fixture.composition.gate.lock().unwrap() = Some(gate.clone());
    let capture = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    gate.entered.acquire().await.unwrap().forget();
    fixture
        .kernel
        .submit(SubmitTurn {
            reasoning_effort: None,
            session: SubmitSession::Resume(prepared),
            turn_id: TurnId::new("concurrent-resident").unwrap(),
            text: "resident".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    gate.release.add_permits(1);
    let snapshot = capture.await.unwrap().unwrap();
    assert_eq!(snapshot.generation_sha256(), "c".repeat(64));
    assert_eq!(snapshot.entries()[0].view().unwrap().value(), &false);
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn capture_deadline_includes_cold_generation_resolution_and_releases_admission() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let gate = PinGate::new();
    *fixture.composition.gate.lock().unwrap() = Some(gate.clone());
    let capture = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    gate.entered.acquire().await.unwrap().forget();
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(capture.await.unwrap().is_err());
    assert_eq!(fixture.projection.entered.available_permits(), 0);
    assert!(
        fixture
            .kernel
            .projection_snapshot(&fixture.session_id)
            .await
            .is_ok()
    );
    fixture.stop().await;
}

#[tokio::test]
async fn projection_commit_hints_are_scoped_coalesced_and_end_on_shutdown() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let mut changes = fixture
        .kernel
        .watch_projection_changes(&fixture.session_id)
        .unwrap();
    let mut unrelated = fixture
        .kernel
        .watch_projection_changes(&SessionId::new("unpublished-other").unwrap())
        .unwrap();
    for (id, value) in [("on", true), ("off", false)] {
        let invocation = fixture.invocation(id, value).await;
        fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                invocation,
            )
            .await
            .unwrap();
    }
    changes.next().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), changes.next())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), unrelated.next())
            .await
            .is_err()
    );
    fixture.kernel.shutdown(fixture.workers).await.unwrap();
    assert!(changes.next().await.is_none());
    assert!(unrelated.next().await.is_none());
    assert!(fixture.runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
pub(super) struct ToggleView {
    handle: DomainHandle<bool>,
    blocked: AtomicBool,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    tokens: std::sync::Mutex<Vec<CancellationToken>>,
}
impl ToggleView {
    pub(super) fn new(handle: DomainHandle<bool>) -> Self {
        Self {
            handle,
            blocked: AtomicBool::new(false),
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            tokens: std::sync::Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl SessionProjection for ToggleView {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        token: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        self.tokens.lock().unwrap().push(token);
        self.entered.add_permits(1);
        if self.blocked.load(Ordering::SeqCst) {
            self.release.acquire().await.unwrap().forget();
        }
        let value = self
            .handle
            .decode(&context.domains()[0].snapshot)
            .map_err(|error| ContributionError::Invalid(error.to_string()))?;
        ProjectionValue::new(value.into())
            .map_err(|error| ContributionError::Invalid(error.to_string()))
    }
}

#[tokio::test]
async fn idle_commands_advance_the_projection_control_cut_without_manufacturing_facts() {
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let fixture = Fixture::start(store, false).await;
        let before = fixture
            .kernel
            .projection_snapshot(&fixture.session_id)
            .await
            .unwrap();
        assert_eq!(before.entries()[0].view().unwrap().value(), &false);
        let input = fixture.invocation("projection-toggle", true).await;
        fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                input,
            )
            .await
            .unwrap();
        let after = fixture
            .kernel
            .projection_snapshot(&fixture.session_id)
            .await
            .unwrap();
        assert_eq!(after.entries()[0].view().unwrap().value(), &true);
        assert!(after.cursor().can_follow(before.cursor()));
        let (
            ProjectionCursor::Durable {
                fact_seq: old_fact,
                control_seq: old_control,
            },
            ProjectionCursor::Durable {
                fact_seq,
                control_seq,
            },
        ) = (before.cursor(), after.cursor())
        else {
            panic!("durable snapshots")
        };
        assert_eq!(fact_seq, old_fact);
        assert_eq!(control_seq, old_control + 1);
        assert_eq!(after.generation_sha256(), "c".repeat(64));
        assert_eq!(
            after.header_sha256(),
            header("command-session").fingerprint().unwrap()
        );
        fixture.stop().await;
    }
}

#[tokio::test]
async fn snapshot_is_one_captured_cut_even_when_a_command_commits_during_its_callback() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let input = fixture.invocation("racing-view", true).await;
    let before = fixture
        .store
        .read_domain_states(&fixture.session_id, None)
        .await
        .unwrap();
    fixture.projection.blocked.store(true, Ordering::SeqCst);
    let task = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    fixture.projection.entered.acquire().await.unwrap().forget();
    fixture
        .kernel
        .execute(
            fixture
                .kernel
                .prepare_resume(&fixture.session_id)
                .await
                .unwrap(),
            input,
        )
        .await
        .unwrap();
    let newer = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    fixture.projection.entered.acquire().await.unwrap().forget();
    fixture.projection.release.add_permits(2);
    let captured = task.await.unwrap().unwrap();
    let newer = newer.await.unwrap().unwrap();
    assert_eq!(newer.entries()[0].view().unwrap().value(), &true);
    assert_ne!(captured.cursor(), newer.cursor());
    assert_eq!(
        captured.cursor(),
        ProjectionCursor::Durable {
            fact_seq: before.durable_fact_seq,
            control_seq: before.durable_control_seq
        }
    );
    assert_eq!(captured.entries()[0].view().unwrap().value(), &false);
    fixture.projection.blocked.store(false, Ordering::SeqCst);
    let current = fixture
        .kernel
        .projection_snapshot(&fixture.session_id)
        .await
        .unwrap();
    assert_eq!(current.entries()[0].view().unwrap().value(), &true);
    assert_ne!(captured.cursor(), current.cursor());
    fixture.stop().await;
}

#[tokio::test]
async fn cold_projection_does_not_hydrate_or_require_unrelated_domain_codecs() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    // The completed Session is cold. Publish a new generation with no executable domain catalog.
    let old = fixture.composition.pin.read().unwrap().clone();
    let pin = AgentCompositionPin::new(
        old.preset_id().clone(),
        "d".repeat(64),
        old.tools(),
        old.context_builder(),
        DomainCatalog::default(),
        old.contributions().clone(),
        Arc::new(()),
    )
    .unwrap();
    *fixture.composition.pin.write().unwrap() = pin;
    let calls = fixture.composition.calls.load(Ordering::SeqCst);
    for _ in 0..2 {
        let snapshot = fixture
            .kernel
            .projection_snapshot(&fixture.session_id)
            .await
            .unwrap();
        assert_eq!(snapshot.generation_sha256(), "d".repeat(64));
        assert_eq!(snapshot.entries()[0].view().unwrap().value(), &false);
    }
    assert_eq!(
        fixture.composition.calls.load(Ordering::SeqCst),
        calls + 2,
        "read capture must not publish a resident execution"
    );
    assert!(
        fixture
            .kernel
            .prepare_resume(&fixture.session_id)
            .await
            .is_err()
    );
    assert!(
        !fixture
            .store
            .read_facts(
                &fixture.session_id,
                0,
                16,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES
            )
            .await
            .unwrap()
            .facts
            .is_empty()
    );
    fixture.stop().await;
}

#[tokio::test]
async fn resident_projection_keeps_the_original_pin_when_current_generation_is_unavailable() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    fixture
        .kernel
        .submit(SubmitTurn {
            reasoning_effort: None,
            session: SubmitSession::Resume(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
            ),
            turn_id: TurnId::new("resident").unwrap(),
            text: "stay resident".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    fixture.composition.reject.store(true, Ordering::SeqCst);
    let calls = fixture.composition.calls.load(Ordering::SeqCst);
    let snapshot = fixture
        .kernel
        .projection_snapshot(&fixture.session_id)
        .await
        .unwrap();
    assert_eq!(snapshot.generation_sha256(), "c".repeat(64));
    assert_eq!(fixture.composition.calls.load(Ordering::SeqCst), calls);
    fixture.stop().await;
}

#[tokio::test]
async fn pending_capture_capacity_and_drop_shutdown_release_callback_ownership() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    store
        .watermark_reads
        .forget_permits(store.watermark_reads.available_permits());
    let reads = store.projection_reads.load(Ordering::SeqCst);
    fixture.projection.blocked.store(true, Ordering::SeqCst);
    let mut tasks = Vec::new();
    for _ in 0..16 {
        tasks.push(tokio::spawn({
            let kernel = fixture.kernel.clone();
            let id = fixture.session_id.clone();
            async move { kernel.projection_snapshot(&id).await }
        }));
    }
    store
        .watermark_reads
        .acquire_many(16)
        .await
        .unwrap()
        .forget();
    fixture.projection.entered.acquire().await.unwrap().forget();
    assert_eq!(store.projection_reads.load(Ordering::SeqCst) - reads, 1);
    assert_eq!(fixture.projection.tokens.lock().unwrap().len(), 1);
    assert!(matches!(
        fixture
            .kernel
            .projection_snapshot(&fixture.session_id)
            .await,
        Err(TurnError::ProjectionCapacity)
    ));
    let cancelled = tasks.pop().unwrap();
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    assert!(!fixture.projection.tokens.lock().unwrap()[0].is_cancelled());
    fixture.projection.release.add_permits(1);
    let mut first = None;
    for task in tasks {
        let snapshot = task.await.unwrap().unwrap();
        if let Some(first) = &first {
            assert_eq!(&snapshot, first);
        }
        first = Some(snapshot);
    }
    // A completed flight must never become a result cache, even at the same cut.
    let task = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    fixture.projection.entered.acquire().await.unwrap().forget();
    assert_eq!(store.projection_reads.load(Ordering::SeqCst) - reads, 2);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(fixture.projection.tokens.lock().unwrap()[1].is_cancelled());
    let task = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    fixture.projection.entered.acquire().await.unwrap().forget();
    fixture.kernel.shutdown(fixture.workers).await.unwrap();
    assert!(matches!(task.await.unwrap(), Err(TurnError::ShuttingDown)));
    assert!(
        fixture
            .projection
            .tokens
            .lock()
            .unwrap()
            .iter()
            .all(CancellationToken::is_cancelled)
    );
    assert!(fixture.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn projection_rejects_a_historical_page_returned_for_current_state() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    store.stale_domain_read.store(true, Ordering::Release);
    assert!(
        fixture
            .kernel
            .projection_snapshot(&fixture.session_id)
            .await
            .is_err()
    );
    assert_eq!(fixture.projection.entered.available_permits(), 0);
    store.stale_domain_read.store(false, Ordering::Release);
    fixture
        .kernel
        .projection_snapshot(&fixture.session_id)
        .await
        .unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn same_digest_different_generation_flights_keep_the_sixteen_caller_bound() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    fixture.projection.blocked.store(true, Ordering::SeqCst);
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let old = fixture.composition.pin.read().unwrap().clone();
        *fixture.composition.pin.write().unwrap() = AgentCompositionPin::new(
            old.preset_id().clone(),
            old.source_digest(),
            old.tools(),
            old.context_builder(),
            old.domains().clone(),
            old.contributions().clone(),
            Arc::new(()),
        )
        .unwrap();
        tasks.push(tokio::spawn({
            let kernel = fixture.kernel.clone();
            let id = fixture.session_id.clone();
            async move { kernel.projection_snapshot(&id).await }
        }));
        fixture.projection.entered.acquire().await.unwrap().forget();
    }
    assert_eq!(fixture.projection.tokens.lock().unwrap().len(), 16);
    assert!(matches!(
        fixture
            .kernel
            .projection_snapshot(&fixture.session_id)
            .await,
        Err(TurnError::ProjectionCapacity)
    ));
    for task in tasks {
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
    assert!(
        fixture
            .projection
            .tokens
            .lock()
            .unwrap()
            .iter()
            .all(CancellationToken::is_cancelled)
    );
    fixture.projection.blocked.store(false, Ordering::SeqCst);
    fixture
        .kernel
        .projection_snapshot(&fixture.session_id)
        .await
        .unwrap();
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn joining_waiter_cannot_extend_the_creators_capture_deadline() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    fixture
        .kernel
        .submit(SubmitTurn {
            reasoning_effort: None,
            session: SubmitSession::Resume(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
            ),
            turn_id: TurnId::new("deadline-resident").unwrap(),
            text: "hold pin".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let executor = fixture.kernel.register("deadline-fixture".into()).unwrap();
    let claim = fixture
        .kernel
        .claim("deadline-fixture", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    fixture
        .kernel
        .flush(&claim, claim.accepted_seq())
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    store.pause_domain_read.store(true, Ordering::Release);
    let first = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    store.domain_read_entered.notified().await;
    tokio::time::advance(Duration::from_secs(20)).await;
    store
        .watermark_reads
        .forget_permits(store.watermark_reads.available_permits());
    let second = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    store.watermark_reads.acquire().await.unwrap().forget();
    tokio::time::advance(Duration::from_secs(11)).await;
    assert!(first.await.unwrap().is_err());
    assert!(second.await.unwrap().is_err());
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(31),
        "joining must not wait for its own later deadline"
    );
    assert!(fixture.projection.tokens.lock().unwrap().is_empty());
    store.release_domain_read.notify_one();
    fixture
        .kernel
        .projection_snapshot(&fixture.session_id)
        .await
        .unwrap();
    fixture
        .kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(executor);
    fixture.stop().await;
}

#[tokio::test]
async fn capture_uses_actual_cut_when_a_commit_follows_its_demand_watermark() {
    use rsi_agent_session_protocol::{
        AgentControlRecord, DomainMutationSource, DomainSnapshot, DomainStateCommit,
        DomainStateUpdate, DomainStateValue,
    };
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    let input = fixture.invocation("watermark-race", true).await;
    let before = store
        .read_domain_states(&fixture.session_id, None)
        .await
        .unwrap();
    let update = DomainStateUpdate::new(
        before.states[0].head.revision,
        DomainSnapshot::new(
            before.states[0].snapshot.identity().clone(),
            DomainStateValue::new(true.into()).unwrap(),
        ),
    )
    .unwrap();
    let commit = DomainStateCommit::new(
        Some(input.request_id.clone()),
        DomainMutationSource::Command { invocation: input },
        vec![update],
    )
    .unwrap();
    store.pause_domain_read.store(true, Ordering::Release);
    let capture = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let id = fixture.session_id.clone();
        async move { kernel.projection_snapshot(&id).await }
    });
    tokio::time::timeout(Duration::from_secs(2), store.domain_read_entered.notified())
        .await
        .expect("capture must reach domain read");
    // The paused worker still owns 36 MiB of the Kernel's 64 MiB read admission.
    // Commit directly so this race does not wait on a second Kernel read first.
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: fixture.session_id.clone(),
                expected_fact_seq: before.durable_fact_seq,
                expected_control_seq: before.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![
                    AgentControlRecord::new(
                        before.durable_control_seq + 1,
                        42,
                        AgentControlRecordBody::DomainStateCommitted { commit },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    store.release_domain_read.notify_one();
    let snapshot = tokio::time::timeout(Duration::from_secs(2), capture)
        .await
        .expect("released capture must finish")
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.cursor(),
        ProjectionCursor::Durable {
            fact_seq: before.durable_fact_seq,
            control_seq: before.durable_control_seq + 1
        }
    );
    assert_eq!(snapshot.entries()[0].view().unwrap().value(), &true);
    fixture.stop().await;
}

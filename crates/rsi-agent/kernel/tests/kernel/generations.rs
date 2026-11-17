use super::*;
use futures_util::FutureExt as _;
use rsi_agent_composition_protocol::{
    AgentGenerationCandidate, AgentGenerationSeed, DomainCatalog, DomainDefinition, DomainHandle,
    DomainReferencePolicy,
};
use rsi_agent_session_protocol::{DomainIdentity, DomainRequestId, DomainRevision};
use rsi_agent_turn_protocol::SessionProjections as _;
use rsi_agent_turn_protocol::{DomainMutation, TurnClaim};

#[path = "generations/revisions.rs"]
mod revisions;

#[derive(Debug)]
struct Source {
    selected: Mutex<AgentCompositionPin>,
    seeds: Arc<Mutex<Vec<AgentGenerationSeed>>>,
    stopped: std::sync::atomic::AtomicBool,
    prepare_error: Mutex<Option<AgentCompositionError>>,
    probes: AtomicUsize,
    finish_during_preparation: Mutex<Option<(AgentKernel, TurnClaim)>>,
    preparation: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    build_gate: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}

impl Source {
    fn new(pin: AgentCompositionPin) -> Self {
        Self {
            selected: Mutex::new(pin),
            seeds: Arc::default(),
            stopped: false.into(),
            prepare_error: Mutex::new(None),
            probes: AtomicUsize::new(0),
            finish_during_preparation: Mutex::new(None),
            preparation: Mutex::new(None),
            build_gate: Mutex::new(None),
        }
    }
    fn select(&self, pin: AgentCompositionPin) {
        *self.selected.lock().unwrap() = pin;
    }
}

#[derive(Debug)]
struct Candidate {
    pin: AgentCompositionPin,
    seeds: Arc<Mutex<Vec<AgentGenerationSeed>>>,
    gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}
#[async_trait]
impl AgentGenerationCandidate for Candidate {
    fn selection(&self) -> &rsi_agent_session_protocol::GenerationSelection {
        self.pin.selection()
    }

    async fn build(
        self: Box<Self>,
        seed: &AgentGenerationSeed,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        self.seeds.lock().unwrap().push(seed.clone());
        if let Some((entered, release)) = self.gate {
            entered.notify_one();
            release.notified().await;
        }
        Ok(self.pin)
    }
}
#[async_trait]
impl AgentComposition for Source {
    async fn default_preset_id(&self) -> rsi_agent_composition_protocol::Result<AgentPresetId> {
        Ok(AgentPresetId::new("test-agent").unwrap())
    }
    async fn pin(
        &self,
        _: &AgentPresetId,
        seed: Option<&AgentGenerationSeed>,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        if let Some(seed) = seed {
            self.seeds.lock().unwrap().push(seed.clone());
        }
        Ok(self.selected.lock().unwrap().clone())
    }
    async fn prepare_update(
        &self,
        _: &SessionHeader,
        current: &AgentCompositionPin,
    ) -> rsi_agent_composition_protocol::Result<Option<Box<dyn AgentGenerationCandidate>>> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        let finish = self.finish_during_preparation.lock().unwrap().take();
        if let Some((kernel, claim)) = finish {
            kernel
                .finish_turn(&claim, &TurnOutcome::Completed)
                .await
                .unwrap();
        }
        if let Some(error) = self.prepare_error.lock().unwrap().clone() {
            return Err(error);
        }
        let gate = self.preparation.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.notify_one();
            release.notified().await;
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err(AgentCompositionError::ShuttingDown);
        }
        let next = self.selected.lock().unwrap().clone();
        Ok((next.selection() != current.selection()).then(|| {
            Box::new(Candidate {
                pin: next,
                seeds: self.seeds.clone(),
                gate: self.build_gate.lock().unwrap().take(),
            }) as Box<dyn AgentGenerationCandidate>
        }))
    }
}

#[tokio::test(start_paused = true)]
async fn generation_source_preparation_has_a_finite_deadline() {
    let store = Arc::new(MemoryStore::new());
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(store, source.clone()).await;
    *source.preparation.lock().unwrap() = Some((
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    ));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(31),
        run.kernel.prepare_composition(&run.claim),
    )
    .await;
    assert!(
        matches!(result, Ok(Err(TurnError::Composition(reason))) if reason.contains("30 seconds"))
    );
    assert!(
        run.kernel
            .generation_status(run.claim.session_id())
            .await
            .unwrap()
            .unwrap()
            .rejection
            .unwrap()
            .contains("30 seconds")
    );
    source.select(pin('b', 1, false, false).0);
    assert!(
        run.kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .is_none()
    );
    assert!(source.seeds.lock().unwrap().is_empty());
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let prepared = run
        .kernel
        .prepare_resume(run.claim.session_id())
        .await
        .unwrap();
    run.kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(prepared),
            turn_id: client_turn_id(),
            text: "retry".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let claim = run
        .kernel
        .claim("generation-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    if let Some(candidate) = run.kernel.prepare_composition(&claim).await.unwrap() {
        candidate.commit().await.unwrap();
    }
    assert!(
        run.kernel
            .composition(&claim)
            .unwrap()
            .same_generation(&source.selected.lock().unwrap())
    );
    assert!(!source.seeds.lock().unwrap().is_empty());
    run.stop().await;
}

#[tokio::test(start_paused = true)]
async fn generation_build_timeout_is_visible_and_does_not_repeat_within_the_turn() {
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(Arc::new(MemoryStore::new()), source.clone()).await;
    source.select(pin('b', 1, false, false).0);
    *source.build_gate.lock().unwrap() = Some((
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    ));
    assert!(matches!(
        run.kernel.prepare_composition(&run.claim).await,
        Err(TurnError::Composition(_))
    ));
    assert!(
        run.kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .is_none()
    );
    source.select(pin('c', 1, false, false).0);
    assert!(
        run.kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(source.seeds.lock().unwrap().len(), 1);
    run.stop().await;
}

#[tokio::test(start_paused = true)]
async fn generation_capture_flush_does_not_block_cancellation_admission() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(store.clone(), source.clone()).await;
    source.select(pin('b', 1, false, false).0);
    store.pause_next_append();
    run.kernel
        .publish(
            &run.claim,
            vec![SessionFactBody::StepStarted {
                turn_id: run.claim.turn_id().clone(),
                step_id: StepId::new("before-generation").unwrap(),
            }],
        )
        .await
        .unwrap();
    store.wait_until_append_is_blocked().await;
    let mut preparing = Box::pin(run.kernel.prepare_composition(&run.claim));
    assert!(futures_util::poll!(&mut preparing).is_pending());
    let mut cancelling = Box::pin(run.kernel.cancel(
        run.claim.session_id(),
        run.claim.turn_id(),
        None,
    ));
    assert!(futures_util::poll!(&mut cancelling).is_pending());
    // Cancellation admission must reach the resident flag while Store is
    // blocked. Its token and acknowledgement still require durable publication.
    let repeated = run
        .kernel
        .cancel(run.claim.session_id(), run.claim.turn_id(), None)
        .now_or_never()
        .expect("cancellation must not queue behind generation capture")
        .unwrap();
    assert!(!repeated.accepted && !repeated.already_terminal);
    store.release_blocked_append();
    assert!(cancelling.await.unwrap().accepted);
    assert!(matches!(preparing.await, Err(TurnError::Cancelled)));
    run.stop().await;
}

#[tokio::test]
async fn repeated_rejection_skips_build_until_the_domain_cut_changes() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let (old, handle) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old));
    let run = Run::start(store.clone(), source.clone()).await;
    run.kernel
        .commit_domains(&run.claim, mutation(&handle, 1, "true"))
        .await
        .unwrap();
    source.select(pin('b', 1, false, true).0);
    assert!(matches!(
        run.kernel.prepare_composition(&run.claim).await,
        Err(TurnError::Composition(_))
    ));
    let built = source.seeds.lock().unwrap().len();
    let reads = store.projection_reads.load(Ordering::SeqCst);
    assert!(
        run.kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(source.seeds.lock().unwrap().len(), built);
    assert_eq!(
        store.projection_reads.load(Ordering::SeqCst),
        reads,
        "memo check must not read Domain payloads"
    );
    run.kernel
        .commit_domains(&run.claim, mutation(&handle, 2, "changed"))
        .await
        .unwrap();
    assert!(matches!(
        run.kernel.prepare_composition(&run.claim).await,
        Err(TurnError::Composition(_))
    ));
    assert_eq!(source.seeds.lock().unwrap().len(), built + 1);
    run.stop().await;
}

#[tokio::test(start_paused = true)]
async fn cancellation_preempts_a_stalled_candidate_build() {
    let old = pin('a', 1, false, false).0;
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(Arc::new(MemoryStore::new()), source.clone()).await;
    source.select(pin('b', 1, false, false).0);
    let entered = Arc::new(tokio::sync::Notify::new());
    *source.build_gate.lock().unwrap() =
        Some((entered.clone(), Arc::new(tokio::sync::Notify::new())));
    let preparing = tokio::spawn({
        let kernel = run.kernel.clone();
        let claim = run.claim.clone();
        async move { kernel.prepare_composition(&claim).await }
    });
    entered.notified().await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        assert!(
            run.kernel
                .cancel(run.claim.session_id(), run.claim.turn_id(), None)
                .await
                .unwrap()
                .accepted
        );
        assert!(matches!(
            preparing.await.unwrap(),
            Err(TurnError::Cancelled)
        ));
    })
    .await;
    assert!(
        result.is_ok(),
        "cancellation must not queue behind candidate build"
    );
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    run.stop().await;
}

#[tokio::test]
async fn prepared_candidate_cannot_overwrite_a_concurrent_domain_write() {
    let (old, handle) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(Arc::new(MemoryStore::new()), source.clone()).await;
    let next = pin('b', 1, false, false).0;
    source.select(next.clone());
    let candidate = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap();
    run.kernel
        .commit_domains(&run.claim, mutation(&handle, 1, "concurrent"))
        .await
        .unwrap();
    assert!(matches!(
        candidate.commit().await,
        Err(TurnError::Composition(_))
    ));
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    let candidate = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        source.seeds.lock().unwrap().last().unwrap().states()[0]
            .state()
            .value(),
        &serde_json::json!(true)
    );
    assert!(candidate.commit().await.unwrap().same_generation(&next));
    run.stop().await;
}

#[tokio::test(start_paused = true)]
async fn source_preparation_does_not_block_domain_writes_and_build_observes_the_latest_state() {
    let store = Arc::new(MemoryStore::new());
    let (old, handle) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old));
    let run = Run::start(store, source.clone()).await;
    source.select(pin('b', 1, false, false).0);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *source.preparation.lock().unwrap() = Some((entered.clone(), release.clone()));
    let preparing = tokio::spawn({
        let kernel = run.kernel.clone();
        let claim = run.claim.clone();
        async move { kernel.prepare_composition(&claim).await }
    });
    entered.notified().await;
    let written = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        run.kernel
            .commit_domains(&run.claim, mutation(&handle, 1, "during-prepare")),
    )
    .await;
    release.notify_one();
    let candidate = preparing.await.unwrap().unwrap().unwrap();
    assert!(
        written.is_ok(),
        "source compilation must not hold Session mutation admission"
    );
    written.unwrap().unwrap();
    assert_eq!(
        source.seeds.lock().unwrap().last().unwrap().states()[0]
            .state()
            .value(),
        &serde_json::json!(true)
    );
    candidate.commit().await.unwrap();
    run.stop().await;
}

fn pin(
    digit: char,
    version: u32,
    include_extra: bool,
    reject_true: bool,
) -> (AgentCompositionPin, DomainHandle<bool>) {
    let definition = DomainDefinition::new(
        DomainIdentity::new("fixture.state", version).unwrap(),
        &false,
        if reject_true {
            reject_true_state
        } else {
            accept_state
        },
        DomainReferencePolicy::None,
    )
    .unwrap();
    let extra = DomainDefinition::new(
        DomainIdentity::new("fixture.extra", 1).unwrap(),
        &false,
        accept_state,
        DomainReferencePolicy::None,
    )
    .unwrap();
    let mut definitions = vec![definition.registration()];
    if include_extra {
        definitions.push(extra.registration());
    }
    let domains = DomainCatalog::new(definitions).unwrap();
    let handle = domains.bind(&definition).unwrap();
    let pin = AgentCompositionPin::new(
        AgentPresetId::new("test-agent").unwrap(),
        digit.to_string().repeat(64),
        Arc::new(SourceOnlyTools),
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        domains,
        rsi_agent_composition_protocol::ContributionCatalog::default(),
        Arc::new(()),
    )
    .unwrap();
    (pin, handle)
}
#[allow(clippy::trivially_copy_pass_by_ref, clippy::unnecessary_wraps)] // Exact Domain codec function-pointer signature.
fn accept_state(_: &bool) -> Result<(), String> {
    Ok(())
}
#[allow(clippy::trivially_copy_pass_by_ref)] // Exact Domain codec function-pointer signature.
fn reject_true_state(value: &bool) -> Result<(), String> {
    if *value {
        Err("true is unsupported".into())
    } else {
        Ok(())
    }
}
fn mutation(handle: &DomainHandle<bool>, revision: u64, id: &str) -> DomainMutation {
    DomainMutation {
        guards: vec![],
        require_uncancelled_turn: false,
        request_id: DomainRequestId::new(id).unwrap(),
        proposals: vec![
            handle
                .propose(DomainRevision::new(revision), &true)
                .unwrap(),
        ],
        facts: vec![],
    }
}

#[derive(Debug)]
struct ValidateState {
    error: Mutex<Option<rsi_agent_composition_protocol::ContributionError>>,
    calls: Mutex<Vec<(SessionId, DomainRevision, serde_json::Value)>>,
}

#[derive(Debug, Default)]
struct StalledValidation {
    entered: tokio::sync::Notify,
    cancellation: Mutex<Option<CancellationToken>>,
    stalled: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl rsi_agent_composition_protocol::GenerationValidator for StalledValidation {
    async fn validate(
        &self,
        _: &SessionHeader,
        _: &[rsi_agent_session_protocol::DomainStateView],
        cancellation: CancellationToken,
    ) -> rsi_agent_composition_protocol::ContributionResult<()> {
        if self.stalled.load(Ordering::Acquire) {
            *self.cancellation.lock().unwrap() = Some(cancellation.clone());
            self.entered.notify_one();
            cancellation.cancelled().await;
        }
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
#[expect(
    clippy::too_many_lines,
    reason = "One retained validator proves deadline and caller cancellation release admission before a successful retry"
)]
async fn cold_validation_deadline_and_dropped_submission_cancel_callbacks_and_release_admission() {
    use rsi_agent_composition_protocol::{
        ContributionCatalog, ContributionKind, ContributionRegistration,
    };
    let runtime = Runtime::default();
    let (owner, context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let (position, _registration) = context
        .registration_context()
        .unwrap()
        .register("cold validation", || Ok(()), Ok)
        .unwrap();
    let validator = Arc::new(StalledValidation::default());
    let callbacks = ContributionCatalog::freeze(vec![(
        ContributionRegistration::new(
            rsi_agent_session_protocol::ContributionId::new("fixture.stalled").unwrap(),
            0,
            ContributionKind::GenerationValidation(validator.clone()),
        ),
        position,
    )])
    .unwrap();
    let base = pin('b', 1, false, false).0;
    let next = AgentCompositionPin::new(
        AgentPresetId::new("test-agent").unwrap(),
        "b".repeat(64),
        base.tools(),
        base.context_builder(),
        base.domains().clone(),
        callbacks,
        Arc::new(()),
    )
    .unwrap();
    let store = Arc::new(MemoryStore::new());
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(store.clone(), source.clone()).await;
    let session = run.claim.session_id().clone();
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    run.stop().await;
    source.select(next);
    let kernel = AgentKernel::recover_with_clock(store, source, Arc::new(FixedClock))
        .await
        .unwrap();
    let workers = kernel.start_workers();
    for abort in [true, false] {
        validator.stalled.store(true, Ordering::Release);
        let preparing = tokio::spawn({
            let kernel = kernel.clone();
            let session = session.clone();
            async move { kernel.prepare_resume(&session).await }
        });
        validator.entered.notified().await;
        if abort {
            preparing.abort();
            assert!(preparing.await.unwrap_err().is_cancelled());
        } else {
            assert!(
                matches!(preparing.await.unwrap(), Err(TurnError::Composition(reason)) if reason.contains("30 seconds"))
            );
        }
        assert!(
            validator
                .cancellation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled()
        );
    }
    for abort in [true, false] {
        validator.stalled.store(false, Ordering::Release);
        let prepared = kernel.prepare_resume(&session).await.unwrap();
        validator.stalled.store(true, Ordering::Release);
        let submitting = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                kernel
                    .submit(SubmitTurn {
                        session: SubmitSession::Resume(prepared),
                        turn_id: client_turn_id(),
                        text: "resume".into(),
                        model: None,
                        reasoning_effort: None,
                        sandbox: None,
                    })
                    .await
            }
        });
        validator.entered.notified().await;
        if abort {
            submitting.abort();
            assert!(submitting.await.unwrap_err().is_cancelled());
        } else {
            assert!(
                matches!(submitting.await.unwrap(), Err(TurnError::Composition(reason)) if reason.contains("30 seconds"))
            );
        }
        assert!(
            validator
                .cancellation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled()
        );
    }
    validator.stalled.store(false, Ordering::Release);
    let prepared = kernel.prepare_resume(&session).await.unwrap();
    kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(prepared),
            turn_id: client_turn_id(),
            text: "retry after validation".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    kernel.shutdown(workers).await.unwrap();
    assert!(owner.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}
#[async_trait]
impl rsi_agent_composition_protocol::GenerationValidator for ValidateState {
    async fn validate(
        &self,
        header: &SessionHeader,
        domains: &[rsi_agent_session_protocol::DomainStateView],
        cancellation: CancellationToken,
    ) -> rsi_agent_composition_protocol::ContributionResult<()> {
        assert!(!cancellation.is_cancelled());
        self.calls.lock().unwrap().push((
            header.session_id().clone(),
            domains[0].revision,
            domains[0].snapshot.state().value().clone(),
        ));
        if let Some(error) = self.error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(())
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One candidate follows content rejection, transport loss, capacity, discard and proven publication without changing its predecessor"
)]
async fn candidate_private_state_must_validate_before_selection_and_revalidate_after_discard() {
    use rsi_agent_composition_protocol::{
        ContributionCatalog, ContributionError, ContributionKind, ContributionRegistration,
    };
    let runtime = Runtime::default();
    let (owner, context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let (position, _registration) = context
        .registration_context()
        .unwrap()
        .register("candidate state fixture", || Ok(()), Ok)
        .unwrap();
    let validator = Arc::new(ValidateState {
        error: Mutex::new(Some(ContributionError::Invalid(
            "unsupported checkpoint".into(),
        ))),
        calls: Mutex::new(vec![]),
    });
    let callbacks = ContributionCatalog::freeze(vec![(
        ContributionRegistration::new(
            rsi_agent_session_protocol::ContributionId::new("fixture.validate").unwrap(),
            0,
            ContributionKind::GenerationValidation(validator.clone()),
        ),
        position,
    )])
    .unwrap();
    let (old, handle) = pin('a', 1, false, false);
    let (next, _) = pin('b', 1, false, false);
    let next = AgentCompositionPin::new(
        AgentPresetId::new("test-agent").unwrap(),
        "b".repeat(64),
        next.tools(),
        next.context_builder(),
        next.domains().clone(),
        callbacks,
        Arc::new(()),
    )
    .unwrap();
    let source = Arc::new(Source::new(old.clone()));
    let store = Arc::new(MemoryStore::new());
    let run = Run::start(store.clone(), source.clone()).await;
    run.kernel
        .commit_domains(&run.claim, mutation(&handle, 1, "committed-state"))
        .await
        .unwrap();
    source.select(next.clone());
    let before = store.read_watermarks(run.claim.session_id()).await.unwrap();
    assert!(
        matches!(run.kernel.prepare_composition(&run.claim).await, Err(TurnError::Composition(message)) if message.contains("unsupported checkpoint"))
    );
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    assert_eq!(
        store.read_watermarks(run.claim.session_id()).await.unwrap(),
        before
    );
    // Repairing the rejected candidate changes its executable selection. Pure
    // transport/capacity failures below remain retryable without another edit.
    let next = next.with_selection(
        rsi_agent_session_protocol::GenerationSelection::new("d".repeat(64)).unwrap(),
    );
    source.select(next.clone());
    for (failure, expected) in [
        (ContributionError::Closed, TurnError::ExecutionUnavailable),
        (ContributionError::Capacity, TurnError::Capacity),
    ] {
        *validator.error.lock().unwrap() = Some(failure);
        assert_eq!(
            run.kernel
                .prepare_composition(&run.claim)
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(
            store.read_watermarks(run.claim.session_id()).await.unwrap(),
            before
        );
    }
    *validator.error.lock().unwrap() = None;
    let discarded = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap();
    drop(discarded);
    assert_eq!(
        store.read_watermarks(run.claim.session_id()).await.unwrap(),
        before
    );
    let candidate = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *validator.calls.lock().unwrap(),
        vec![
            (
                run.claim.session_id().clone(),
                DomainRevision::new(2),
                serde_json::json!(true)
            );
            5
        ]
    );
    assert!(candidate.commit().await.unwrap().same_generation(&next));
    let committed = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(committed.states[0].head.revision, DomainRevision::new(2));
    run.stop().await;
    assert!(owner.dispose().await.is_clean());
}

struct Run {
    kernel: AgentKernel,
    workers: rsi_agent_kernel::KernelWorkers,
    lease: rsi_agent_turn_protocol::ExecutorLease,
    claim: TurnClaim,
}

#[tokio::test]
async fn retained_draft_owner_cannot_override_the_committed_cold_generation() {
    use rsi_agent_turn_protocol::{ContinuationBinding, SessionContinuations, SessionProjections};
    let store = Arc::new(MemoryStore::new());
    let (old, _) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(store, source.clone()).await;
    let initial = PreparedFreshSession::new(run.claim.header().clone(), old).unwrap();
    let snapshot = initial.baseline().initial_states()[0].clone();
    let draft = SessionContinuations::arm(
        &run.kernel,
        SubmitSession::Fresh(initial),
        ContinuationBinding {
            domain: snapshot.identity().clone(),
            owner: DomainRequestId::new("retained-draft").unwrap(),
            revision: DomainRevision::new(0),
            snapshot_sha256: snapshot.sha256().unwrap(),
        },
    )
    .await
    .unwrap();
    let (next, _) = pin('b', 1, false, false);
    source.select(next.clone());
    run.kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap()
        .commit()
        .await
        .unwrap();
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert!(matches!(
        run.kernel
            .resident_composition(run.claim.session_id())
            .unwrap(),
        rsi_agent_turn_protocol::ResidentComposition::NotResident
    ));
    let current = pin('c', 1, false, false).0;
    source.select(current.clone());
    let prepared = run
        .kernel
        .prepare_resume(run.claim.session_id())
        .await
        .unwrap();
    run.kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(prepared),
            turn_id: client_turn_id(),
            text: "resume the committed generation".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let resumed = run
        .kernel
        .claim("generation-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(
        run.kernel
            .composition(&resumed)
            .unwrap()
            .same_generation(&current)
    );
    drop(draft);
    run.stop().await;
}
#[tokio::test]
#[allow(clippy::too_many_lines)] // Retain two public leases across selection publication, eviction and cold admission.
async fn cold_composition_finds_the_matching_selection_after_an_older_domain_lease() {
    use rsi_agent_turn_protocol::{ContinuationBinding, SessionContinuations};
    let store = Arc::new(MemoryStore::new());
    let old = pin('a', 1, true, false).0;
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(store, source.clone()).await;
    let draft = |pin: AgentCompositionPin, domain: &str| {
        let session = PreparedFreshSession::new(run.claim.header().clone(), pin).unwrap();
        let snapshot = session
            .baseline()
            .initial_states()
            .into_iter()
            .find(|snapshot| snapshot.identity().id() == domain)
            .unwrap();
        let binding = ContinuationBinding {
            domain: snapshot.identity().clone(),
            owner: DomainRequestId::new("retained-owner").unwrap(),
            revision: DomainRevision::new(0),
            snapshot_sha256: snapshot.sha256().unwrap(),
        };
        (SubmitSession::Fresh(session), binding)
    };
    let (session, binding) = draft(old.clone(), "fixture.extra");
    let older = SessionContinuations::arm(&run.kernel, session, binding)
        .await
        .unwrap();
    let selected = old.clone().with_selection(
        rsi_agent_session_protocol::GenerationSelection::new("b".repeat(64)).unwrap(),
    );
    assert!(old.same_generation(&selected));
    assert_ne!(old.selection(), selected.selection());
    source.select(selected.clone());
    run.kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap()
        .commit()
        .await
        .unwrap();
    let (session, binding) = draft(selected.clone(), "fixture.state");
    let matching = SessionContinuations::arm(&run.kernel, session, binding)
        .await
        .unwrap();
    assert!(older.is_armed() && matching.is_armed());
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert!(matches!(
        run.kernel
            .resident_composition(run.claim.session_id())
            .unwrap(),
        rsi_agent_turn_protocol::ResidentComposition::NotResident
    ));
    let builds = source.seeds.lock().unwrap().len();
    source.select(pin('c', 1, true, false).0);
    let prepared = run
        .kernel
        .prepare_resume(run.claim.session_id())
        .await
        .unwrap();
    assert_eq!(
        source.seeds.lock().unwrap().len(),
        builds,
        "an earlier mismatched lease must not force cold reconstruction"
    );
    run.kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(prepared),
            turn_id: client_turn_id(),
            text: "resume the retained selection".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let resumed = run
        .kernel
        .claim("generation-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let actual = run.kernel.composition(&resumed).unwrap();
    assert!(actual.same_generation(&selected));
    assert_eq!(actual.selection(), selected.selection());
    drop((older, matching));
    run.stop().await;
}

impl Run {
    async fn start(store: Arc<dyn SessionStore>, source: Arc<Source>) -> Self {
        let initial = PreparedFreshSession::new(
            header("generation"),
            source.selected.lock().unwrap().clone(),
        )
        .unwrap();
        let kernel = AgentKernel::recover_with_clock(store, source, Arc::new(FixedClock))
            .await
            .unwrap();
        let workers = kernel.start_workers();
        kernel
            .submit(SubmitTurn {
                session: SubmitSession::Fresh(initial),
                turn_id: client_turn_id(),
                text: "work".into(),
                model: None,
                reasoning_effort: None,
                sandbox: None,
            })
            .await
            .unwrap();
        let lease = kernel.register("generation-worker".into()).unwrap();
        let claim = kernel
            .claim("generation-worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        Self {
            kernel,
            workers,
            lease,
            claim,
        }
    }
    async fn stop(self) {
        drop(self.lease);
        self.kernel.shutdown(self.workers).await.unwrap();
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One history follows candidate discard, stateful publication and current-plugin cold recovery"
)]
async fn candidate_drop_preserves_selection_and_cold_recovery_validates_current_plugins() {
    let store = Arc::new(MemoryStore::new());
    let (old, old_handle) = pin('a', 1, true, false);
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(store.clone(), source.clone()).await;
    assert!(
        run.kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .is_none()
    );
    run.kernel
        .commit_domains(&run.claim, mutation(&old_handle, 1, "current-state"))
        .await
        .unwrap();
    let (next, next_handle) = pin('b', 1, false, false);
    source.select(next.clone());
    let before = store.read_watermarks(run.claim.session_id()).await.unwrap();
    let candidate = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap();
    assert!(candidate.composition().same_generation(&next));
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    drop(candidate);
    assert_eq!(
        store.read_watermarks(run.claim.session_id()).await.unwrap(),
        before
    );
    let adopted = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap()
        .commit()
        .await
        .unwrap();
    assert!(adopted.same_generation(&next));
    let states = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    assert_eq!(states.states.len(), 1);
    assert_eq!(states.states[0].head.identity.version(), 1);
    assert_eq!(states.states[0].head.revision, DomainRevision::new(2));
    assert_eq!(
        states.states[0].snapshot.state().value(),
        &serde_json::json!(true)
    );
    assert_eq!(
        store
            .read_domain_states(
                run.claim.session_id(),
                Some(before.durable_control_seq),
                None
            )
            .await
            .unwrap()
            .states
            .len(),
        2
    );
    assert!(
        run.kernel
            .commit_domains(&run.claim, mutation(&old_handle, 2, "stale-handle"))
            .await
            .is_err()
    );
    run.kernel
        .commit_domains(&run.claim, mutation(&next_handle, 2, "new-handle"))
        .await
        .unwrap();
    let saved_header = run.claim.header().clone();
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    run.stop().await;
    source.select(pin('c', 3, true, false).0);
    let cold = AgentKernel::recover_with_clock(store, source.clone(), Arc::new(FixedClock))
        .await
        .unwrap();
    assert!(
        cold.prepare_resume(saved_header.session_id())
            .await
            .is_err(),
        "current codec cannot reinterpret an old version"
    );
    let current = pin('c', 1, true, false).0;
    source.select(current.clone());
    let prepared = cold
        .prepare_resume(saved_header.session_id())
        .await
        .unwrap();
    let workers = cold.start_workers();
    cold.submit(SubmitTurn {
        session: SubmitSession::Resume(prepared),
        turn_id: client_turn_id(),
        text: "continue".into(),
        model: None,
        reasoning_effort: None,
        sandbox: None,
    })
    .await
    .unwrap();
    let lease = cold.register("cold-worker".into()).unwrap();
    let claim = cold
        .claim("cold-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert!(cold.composition(&claim).unwrap().same_generation(&current));
    assert_eq!(
        source.seeds.lock().unwrap().last().unwrap().states()[0]
            .state()
            .value(),
        &serde_json::json!(true)
    );
    drop(lease);
    cold.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn a_new_codec_cannot_relabel_retained_state_even_when_its_validator_accepts_it() {
    let store = Arc::new(MemoryStore::new());
    let (old, handle) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(store.clone(), source.clone()).await;
    run.kernel
        .commit_domains(&run.claim, mutation(&handle, 1, "set-true"))
        .await
        .unwrap();
    source.select(pin('b', 2, false, false).0);
    let before = store.read_watermarks(run.claim.session_id()).await.unwrap();
    let error = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap_err();
    assert!(matches!(error, TurnError::Composition(_)), "{error:?}");
    assert!(error.to_string().contains("unsupported domain codec"));
    assert_eq!(
        store.read_watermarks(run.claim.session_id()).await.unwrap(),
        before
    );
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    run.stop().await;
}

#[tokio::test]
async fn incompatible_state_and_unsettled_model_calls_cannot_change_the_generation() {
    use futures_util::StreamExt;
    let store = Arc::new(MemoryStore::new());
    let (old, handle) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(store.clone(), source.clone()).await;
    run.kernel
        .commit_domains(&run.claim, mutation(&handle, 1, "set-true"))
        .await
        .unwrap();
    source.select(pin('b', 1, false, true).0);
    let mut changes = run
        .kernel
        .watch_projection_changes(run.claim.session_id())
        .unwrap();
    let before = store.read_watermarks(run.claim.session_id()).await.unwrap();
    assert!(matches!(
        run.kernel.prepare_composition(&run.claim).await,
        Err(TurnError::Composition(_))
    ));
    assert_eq!(
        store.read_watermarks(run.claim.session_id()).await.unwrap(),
        before
    );
    let status = run
        .kernel
        .generation_status(run.claim.session_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&status.selection, old.selection());
    assert!(status.rejection.unwrap().contains("true is unsupported"));
    tokio::time::timeout(std::time::Duration::from_secs(1), changes.next())
        .await
        .unwrap()
        .unwrap();
    let projection = run
        .kernel
        .projection_snapshot(run.claim.session_id())
        .await
        .unwrap();
    assert!(
        projection
            .generation_rejection()
            .unwrap()
            .contains("true is unsupported")
    );
    source.select(old.clone());
    assert!(
        run.kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        run.kernel
            .generation_status(run.claim.session_id())
            .await
            .unwrap()
            .unwrap()
            .rejection
            .is_none()
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), changes.next())
        .await
        .unwrap()
        .unwrap();
    let cleared = run
        .kernel
        .projection_snapshot(run.claim.session_id())
        .await
        .unwrap();
    assert_eq!(cleared.cursor(), projection.cursor());
    assert!(cleared.generation_rejection().is_none());
    source.select(pin('c', 2, false, false).0);
    tool_origin::publish_model_source(&run.kernel, &run.claim, "pending", "read", &snapshot())
        .await;
    assert!(matches!(
        run.kernel.prepare_composition(&run.claim).await,
        Err(TurnError::Invalid(_))
    ));
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    run.stop().await;
}

#[tokio::test]
async fn publication_admission_serializes_facts_through_resident_installation() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(store.clone(), source.clone()).await;
    let next = pin('b', 1, false, false).0;
    source.select(next.clone());
    let candidate = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap();
    let before = memory
        .read_watermarks(run.claim.session_id())
        .await
        .unwrap()
        .durable_fact_seq;
    store.pause_next_agent_commit_after_apply();
    let committing = tokio::spawn(candidate.commit());
    store.wait_until_agent_commit_is_applied().await;
    let mut publishing = run.kernel.publish(
        &run.claim,
        vec![SessionFactBody::StepStarted {
            turn_id: run.claim.turn_id().clone(),
            step_id: StepId::new("after-generation").unwrap(),
        }],
    );
    assert!(
        futures_util::poll!(&mut publishing).is_pending(),
        "Fact admission must wait for resident installation"
    );
    assert_eq!(
        memory
            .read_watermarks(run.claim.session_id())
            .await
            .unwrap()
            .durable_fact_seq,
        before
    );
    store.release_applied_agent_commit();
    committing.await.unwrap().unwrap();
    let facts = publishing.await.unwrap().published();
    run.kernel
        .flush(&run.claim, facts.last().unwrap().seq())
        .await
        .unwrap();
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&next)
    );
    assert!(
        memory
            .read_watermarks(run.claim.session_id())
            .await
            .unwrap()
            .durable_fact_seq
            > before
    );
    run.stop().await;
}

#[tokio::test]
async fn detached_commit_waiter_cannot_abandon_durable_selection_installation() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let old = pin('a', 1, false, false).0;
    let next = pin('b', 1, true, false).0;
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(store.clone(), source.clone()).await;
    source.select(next.clone());
    let candidate = run
        .kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap();
    store.pause_next_agent_commit_after_apply();
    let waiter = tokio::spawn(candidate.commit());
    store.wait_until_agent_commit_is_applied().await;
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    waiter.abort();
    let _ = waiter.await;
    store.release_applied_agent_commit();
    // The next admitted preparation waits for installation before comparing recipes.
    assert!(
        run.kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&next)
    );
    assert_eq!(
        memory
            .read_domain_states(run.claim.session_id(), None, None)
            .await
            .unwrap()
            .states
            .len(),
        2
    );
    run.stop().await;
}

#[tokio::test]
async fn generation_source_shutdown_is_not_a_rejected_candidate() {
    let old = pin('a', 1, false, false).0;
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(Arc::new(MemoryStore::new()), source.clone()).await;
    source.stopped.store(true, Ordering::Release);
    assert!(matches!(
        run.kernel.prepare_composition(&run.claim).await,
        Err(TurnError::ShuttingDown)
    ));
    assert!(
        run.kernel
            .composition(&run.claim)
            .unwrap()
            .same_generation(&old)
    );
    run.stop().await;
}

#[tokio::test]
async fn cold_recovery_does_not_read_historical_executable_cas() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(store.clone(), source.clone()).await;
    let next = pin('b', 1, true, false).0;
    source.select(next.clone());
    run.kernel
        .prepare_composition(&run.claim)
        .await
        .unwrap()
        .unwrap()
        .commit()
        .await
        .unwrap();
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let current = pin('c', 1, false, false).0;
    source.select(current.clone());
    *store.missing_cas.lock().unwrap() = Some(next.selection().sha256().to_owned());
    assert!(
        run.kernel
            .prepare_resume(run.claim.session_id())
            .await
            .is_ok()
    );
    assert!(matches!(
        run.kernel
            .resident_composition(run.claim.session_id())
            .unwrap(),
        rsi_agent_turn_protocol::ResidentComposition::NotResident
    ));
    *store.missing_cas.lock().unwrap() = None;
    assert_eq!(
        run.kernel
            .projection_snapshot(run.claim.session_id())
            .await
            .unwrap()
            .generation_sha256(),
        current.source_digest()
    );
    run.stop().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Both proof outcomes retain their complete publication, observer and recovery assertions in one fault-injection scenario"
)]
async fn lost_generation_ack_requires_canonical_proof_before_installing_the_pin() {
    for proof_unavailable in [false, true] {
        let memory = Arc::new(MemoryStore::new());
        let store = Arc::new(FactReadRaceStore::new(memory.clone()));
        let old = pin('a', 1, false, false).0;
        let next = pin('b', 1, true, false).0;
        let source = Arc::new(Source::new(old.clone()));
        let run = Run::start(store.clone(), source.clone()).await;
        source.select(next.clone());
        let candidate = run
            .kernel
            .prepare_composition(&run.claim)
            .await
            .unwrap()
            .unwrap();
        store.fail_domain_after_apply.store(true, Ordering::Release);
        let watermarks = memory
            .read_watermarks(run.claim.session_id())
            .await
            .unwrap();
        let mut observer = run
            .kernel
            .observe_session(
                run.claim.session_id(),
                rsi_agent_turn_protocol::ObservationCursor {
                    fact_seq: watermarks.durable_fact_seq,
                    control_seq: watermarks.durable_control_seq,
                },
            )
            .await
            .unwrap();
        // Drive the observer into its subscribed wait before publishing.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), observer.next())
                .await
                .is_err()
        );
        if proof_unavailable {
            *store.control_read_error.lock().unwrap() =
                Some(StoreError::Io("proof unavailable".into()));
        }
        let result = candidate.commit().await;
        let page = memory
            .read_domain_states(run.claim.session_id(), None, None)
            .await
            .unwrap();
        let position = page.generation.unwrap().control_seq;
        let controls = memory
            .read_controls(
                run.claim.session_id(),
                position - 1,
                1,
                rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
            )
            .await
            .unwrap();
        let AgentControlRecordBody::DomainStateCommitted { commit } = controls.records[0].body()
        else {
            panic!("generation selection")
        };
        assert_eq!(
            commit.source().generation_selection(),
            Some(next.selection())
        );
        if proof_unavailable {
            assert!(
                matches!(result, Err(TurnError::Store(message)) if message.contains("requires recovery"))
            );
            assert!(
                run.kernel
                    .composition(&run.claim)
                    .unwrap()
                    .same_generation(&old)
            );
            assert!(run.kernel.prepare_composition(&run.claim).await.is_err());
            drop(run.lease);
            assert!(run.kernel.shutdown(run.workers).await.is_err());
            let current = pin('c', 1, false, false).0;
            source.select(current.clone());
            let cold = AgentKernel::recover_with_clock(memory, source, Arc::new(FixedClock))
                .await
                .unwrap();
            assert!(cold.prepare_resume(run.claim.session_id()).await.is_ok());
            assert_eq!(
                cold.projection_snapshot(run.claim.session_id())
                    .await
                    .unwrap()
                    .generation_sha256(),
                current.source_digest()
            );
            let workers = cold.start_workers();
            cold.shutdown(workers).await.unwrap();
        } else {
            assert!(result.unwrap().same_generation(&next));
            let update =
                tokio::time::timeout(std::time::Duration::from_millis(500), observer.next())
                    .await
                    .expect("proved commit wakes the subscribed observer")
                    .unwrap()
                    .unwrap();
            assert!(matches!(
                update,
                rsi_agent_turn_protocol::SessionObservation::Control { .. }
            ));
            assert!(
                run.kernel
                    .composition(&run.claim)
                    .unwrap()
                    .same_generation(&next)
            );
            run.stop().await;
        }
    }
}

#[tokio::test]
async fn cold_resume_token_rejects_a_changed_domain_cut_before_hydration() {
    use rsi_agent_session_protocol::{
        AgentControlRecord, DomainMutationSource, DomainStateCommit, DomainStateUpdate,
    };
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    let store = Arc::new(MemoryStore::new());
    let (old, _) = pin('a', 1, false, false);
    let source = Arc::new(Source::new(old.clone()));
    let run = Run::start(store.clone(), source).await;
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let prepared = run
        .kernel
        .prepare_resume(run.claim.session_id())
        .await
        .unwrap();
    let page = store
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    let snapshot = rsi_agent_session_protocol::DomainSnapshot::new(
        page.states[0].head.identity.clone(),
        rsi_agent_session_protocol::DomainStateValue::new(serde_json::json!(true)).unwrap(),
    );
    let commit = DomainStateCommit::new(
        None,
        DomainMutationSource::Recovery {
            selection: old.selection().clone(),
        },
        vec![DomainStateUpdate::new(DomainRevision::new(1), snapshot).unwrap()],
    )
    .unwrap();
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: run.claim.session_id().clone(),
                expected_fact_seq: page.durable_fact_seq,
                expected_control_seq: page.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![
                    AgentControlRecord::new(
                        page.durable_control_seq + 1,
                        2,
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
    let result = run
        .kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(prepared),
            turn_id: client_turn_id(),
            text: "stale prepared state".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await;
    assert!(matches!(result, Err(TurnError::Composition(_))));
    assert!(matches!(
        run.kernel
            .resident_composition(run.claim.session_id())
            .unwrap(),
        rsi_agent_turn_protocol::ResidentComposition::NotResident
    ));
    assert_eq!(
        store
            .read_watermarks(run.claim.session_id())
            .await
            .unwrap()
            .durable_fact_seq,
        page.durable_fact_seq
    );
    assert!(
        run.kernel
            .prepare_resume(run.claim.session_id())
            .await
            .is_ok()
    );
    run.stop().await;
}

#[tokio::test]
async fn source_failure_defers_only_deterministic_probes_until_the_next_turn() {
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(Arc::new(MemoryStore::new()), source.clone()).await;
    *source.prepare_error.lock().unwrap() = Some(AgentCompositionError::Capacity);
    for _ in 0..2 {
        assert!(matches!(
            run.kernel.prepare_composition(&run.claim).await,
            Err(TurnError::Capacity)
        ));
    }
    *source.prepare_error.lock().unwrap() =
        Some(AgentCompositionError::InvalidInput("broken source".into()));
    assert!(matches!(
        run.kernel.prepare_composition(&run.claim).await,
        Err(TurnError::Composition(_))
    ));
    let probes = source.probes.load(Ordering::SeqCst);
    *source.prepare_error.lock().unwrap() = None;
    source.select(pin('b', 1, false, false).0);
    for _ in 0..3 {
        assert!(
            run.kernel
                .prepare_composition(&run.claim)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(source.probes.load(Ordering::SeqCst), probes);
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let prepared = run
        .kernel
        .prepare_resume(run.claim.session_id())
        .await
        .unwrap();
    run.kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(prepared),
            turn_id: client_turn_id(),
            text: "retry".into(),
            model: None,
            reasoning_effort: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let claim = run
        .kernel
        .claim("generation-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let _ = run.kernel.prepare_composition(&claim).await.unwrap();
    assert!(source.probes.load(Ordering::SeqCst) > probes);
    run.stop().await;
}

#[tokio::test]
async fn source_rejection_is_not_replaced_by_a_retired_diagnostic_claim() {
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(Arc::new(MemoryStore::new()), source.clone()).await;
    *source.finish_during_preparation.lock().unwrap() =
        Some((run.kernel.clone(), run.claim.clone()));
    *source.prepare_error.lock().unwrap() =
        Some(AgentCompositionError::InvalidInput("broken source".into()));
    assert!(
        matches!(run.kernel.prepare_composition(&run.claim).await,Err(TurnError::Composition(reason)) if reason.contains("broken source"))
    );
    run.stop().await;
}

#[tokio::test]
async fn cold_adoption_reports_a_selection_only_cas_race_as_retriable_rejection() {
    use rsi_agent_session_protocol::{AgentControlRecord, DomainMutationSource, DomainStateCommit};
    use rsi_agent_store_protocol::{AtomicAgentCommit, AtomicSessionAppend};
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let old = pin('a', 1, false, false).0;
    let source = Arc::new(Source::new(old));
    let run = Run::start(store.clone(), source.clone()).await;
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    source.select(pin('b', 1, false, false).0);
    let prepared = run
        .kernel
        .prepare_resume(run.claim.session_id())
        .await
        .unwrap();
    store
        .pause_agent_commit_before_apply
        .store(true, Ordering::Release);
    let submit = tokio::spawn({
        let kernel = run.kernel.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    session: SubmitSession::Resume(prepared),
                    turn_id: client_turn_id(),
                    text: "race".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.agent_commit_before_apply.notified().await;
    let before = memory
        .read_domain_states(run.claim.session_id(), None, None)
        .await
        .unwrap();
    let commit = DomainStateCommit::new(
        None,
        DomainMutationSource::Recovery {
            selection: rsi_agent_session_protocol::GenerationSelection::new("c".repeat(64))
                .unwrap(),
        },
        vec![],
    )
    .unwrap();
    memory
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: run.claim.session_id().clone(),
                expected_fact_seq: before.durable_fact_seq,
                expected_control_seq: before.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![
                    AgentControlRecord::new(
                        before.durable_control_seq + 1,
                        43,
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
    assert_eq!(
        memory
            .read_domain_states(run.claim.session_id(), None, None)
            .await
            .unwrap()
            .domain_control_seq,
        before.domain_control_seq
    );
    store.release_agent_commit_before_apply.notify_one();
    assert!(matches!(
        submit.await.unwrap(),
        Err(TurnError::Composition(_))
    ));
    assert_eq!(
        memory
            .read_watermarks(run.claim.session_id())
            .await
            .unwrap()
            .durable_fact_seq,
        before.durable_fact_seq
    );
    run.stop().await;
}

#[tokio::test]
async fn dropped_cold_publisher_holds_admission_until_resident_installation() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let source = Arc::new(Source::new(pin('a', 1, false, false).0));
    let run = Run::start(store.clone(), source.clone()).await;
    let id = run.claim.session_id().clone();
    run.kernel
        .finish_turn(&run.claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let old = run.kernel.prepare_resume(&id).await.unwrap();
    source.select(pin('b', 1, false, false).0);
    let next = run.kernel.prepare_resume(&id).await.unwrap();
    let submit = |prepared| SubmitTurn {
        session: SubmitSession::Resume(prepared),
        turn_id: client_turn_id(),
        text: "resume".into(),
        model: None,
        reasoning_effort: None,
        sandbox: None,
    };
    store
        .pause_agent_commit_before_apply
        .store(true, Ordering::Release);
    let publishing = tokio::spawn({
        let kernel = run.kernel.clone();
        let request = submit(next);
        async move { kernel.submit(request).await }
    });
    store.agent_commit_before_apply.notified().await;
    publishing.abort();
    assert!(publishing.await.unwrap_err().is_cancelled());
    let mut competing = Box::pin(run.kernel.submit(submit(old)));
    assert!(futures_util::poll!(competing.as_mut()).is_pending());
    assert!(matches!(
        run.kernel.resident_composition(&id).unwrap(),
        rsi_agent_turn_protocol::ResidentComposition::Loading
    ));
    store.release_agent_commit_before_apply.notify_one();
    competing.await.unwrap();
    let claim = run
        .kernel
        .claim("generation-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let resident = run.kernel.composition(&claim).unwrap();
    let durable = memory.read_domain_states(&id, None, None).await.unwrap();
    assert_eq!(resident.selection(), &durable.generation.unwrap().selection);
    assert_eq!(
        resident.selection(),
        pin('b', 1, false, false).0.selection()
    );
    run.stop().await;
}

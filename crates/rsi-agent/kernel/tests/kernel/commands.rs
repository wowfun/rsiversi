use super::*;
use rsi_agent_composition_protocol::{
    ContributionCatalog, ContributionKind, ContributionRegistration, ContributionResult,
    DomainCatalog, DomainDefinition, DomainHandle, SessionCommand, SessionCommandContext,
    SessionCommandRegistration, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContributionId, DomainIdentity, DomainRequestId, SessionCommandDescriptor,
    SessionCommandInvocation,
};
use rsi_agent_turn_protocol::SessionCommands;

#[tokio::test]
async fn current_domain_reads_reject_a_structurally_valid_historical_page() {
    let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
    let fixture = Fixture::start(store.clone(), false).await;
    let historical = store
        .inner
        .read_domain_states(&fixture.session_id, Some(1))
        .await
        .unwrap();
    historical.validate().unwrap();
    assert!(historical.durable_control_seq > historical.selected_control_seq);
    let prepared = fixture
        .kernel
        .prepare_resume(&fixture.session_id)
        .await
        .unwrap();
    store.stale_domain_read.store(true, Ordering::Release);
    assert!(fixture.kernel.list(prepared).await.is_err());
    store.stale_domain_read.store(false, Ordering::Release);
    assert!(
        fixture
            .kernel
            .list(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap()
            )
            .await
            .is_ok()
    );
    fixture.stop().await;
}

#[derive(Debug)]
struct Toggle {
    handle: DomainHandle<bool>,
    calls: AtomicUsize,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    blocked: bool,
}

#[async_trait]
impl SessionCommand for Toggle {
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        _: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        if self.blocked {
            self.release.acquire().await.unwrap().forget();
        }
        let value = arguments.value().as_bool().unwrap();
        Ok(vec![
            self.handle
                .propose(context.domains[0].revision, &value)
                .unwrap(),
        ])
    }
}

#[derive(Debug)]
struct CommandComposition {
    pin: std::sync::RwLock<AgentCompositionPin>,
    calls: AtomicUsize,
    reject: AtomicBool,
    gate: std::sync::Mutex<Option<Arc<projection::PinGate>>>,
}
#[async_trait]
impl AgentComposition for CommandComposition {
    async fn default_preset_id(&self) -> rsi_agent_composition_protocol::Result<AgentPresetId> {
        Ok(self.pin.read().unwrap().preset_id().clone())
    }
    async fn pin(
        &self,
        _: &AgentPresetId,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered.add_permits(1);
            gate.release.acquire().await.unwrap().forget();
            return Err(
                rsi_agent_composition_protocol::AgentCompositionError::InvalidInput(
                    "injected cold failure".into(),
                ),
            );
        }
        if self.reject.load(Ordering::SeqCst) {
            return Err(
                rsi_agent_composition_protocol::AgentCompositionError::InvalidInput(
                    "injected unavailable generation".into(),
                ),
            );
        }
        Ok(self.pin.read().unwrap().clone())
    }
}

struct Fixture {
    runtime: Runtime,
    kernel: AgentKernel,
    workers: rsi_agent_kernel::KernelWorkers,
    callback: Arc<Toggle>,
    projection: Arc<projection::ToggleView>,
    session_id: SessionId,
    store: Arc<dyn SessionStore>,
    composition: Arc<CommandComposition>,
    _lease: rsi_meta::RegistrationLease,
}

impl Fixture {
    async fn start(store: Arc<dyn SessionStore>, blocked: bool) -> Self {
        let runtime = Runtime::default();
        let (_owner, context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
            .await
            .unwrap();
        let (position, lease) = context
            .registration_context()
            .unwrap()
            .register("command fixture", || Ok(()), Ok)
            .unwrap();
        let definition = DomainDefinition::new(
            DomainIdentity::new("fixture.toggle", 1).unwrap(),
            &false,
            |_| Ok(()),
        )
        .unwrap();
        let domains = DomainCatalog::new([definition.registration()]).unwrap();
        let callback = Arc::new(Toggle {
            handle: domains.bind(&definition).unwrap(),
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            blocked,
        });
        let id = ContributionId::new("fixture.command").unwrap();
        let descriptor =
            SessionCommandDescriptor::new(id.clone(), "toggle", "Toggle the fixture state", true)
                .unwrap();
        let projection = Arc::new(projection::ToggleView::new(
            domains.bind(&definition).unwrap(),
        ));
        let commands = ContributionCatalog::freeze(vec![
            (
                ContributionRegistration::new(
                    id,
                    0,
                    ContributionKind::Command(SessionCommandRegistration::new(
                        descriptor,
                        callback.clone(),
                    )),
                ),
                position.clone(),
            ),
            (
                ContributionRegistration::new(
                    ContributionId::new("fixture.projection").unwrap(),
                    0,
                    ContributionKind::Projection(projection.clone()),
                ),
                position,
            ),
        ])
        .unwrap();
        let pin = AgentCompositionPin::new(
            AgentPresetId::new("test-agent").unwrap(),
            "c".repeat(64),
            Arc::new(EmptyTools),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            domains,
            commands,
            Arc::new(()),
        )
        .unwrap();
        let composition = Arc::new(CommandComposition {
            pin: std::sync::RwLock::new(pin.clone()),
            calls: AtomicUsize::new(0),
            reject: AtomicBool::new(false),
            gate: std::sync::Mutex::new(None),
        });
        let kernel = AgentKernel::recover(store.clone(), composition.clone())
            .await
            .unwrap();
        let workers = kernel.start_workers();
        let header = header("command-session");
        let session_id = header.session_id().clone();
        kernel
            .submit(SubmitTurn {
                session: SubmitSession::Fresh(PreparedFreshSession::new(header, pin).unwrap()),
                turn_id: TurnId::new("first").unwrap(),
                text: "fixture".into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        finish_initial_turn(&kernel).await;
        Self {
            runtime,
            kernel,
            workers,
            callback,
            projection,
            session_id,
            store,
            composition,
            _lease: lease,
        }
    }
    async fn invocation(&self, request: &str, value: bool) -> SessionCommandInvocation {
        let view = self
            .kernel
            .list(self.kernel.prepare_resume(&self.session_id).await.unwrap())
            .await
            .unwrap();
        assert_eq!(view.commands().len(), 1);
        SessionCommandInvocation {
            command: view.commands()[0].id().clone(),
            request_id: DomainRequestId::new(request).unwrap(),
            expected_revision: view.revision(),
            arguments: CommandArguments::new(value.into()).unwrap(),
        }
    }
    async fn stop(self) {
        self.kernel.shutdown(self.workers).await.unwrap();
        assert!(self.runtime.shutdown().await.is_clean());
    }
}

async fn finish_initial_turn(kernel: &AgentKernel) {
    // Complete the original Turn so subsequent commands are genuinely idle controls.
    let executor = kernel.register("fixture".into()).unwrap();
    let claim = kernel
        .claim("fixture", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(executor);
}

#[path = "commands/projection.rs"]
mod projection;

#[tokio::test]
async fn commands_commit_idle_state_once_and_query_after_cold_recovery_without_callbacks() {
    for sqlite in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn SessionStore> = if sqlite {
            Arc::new(rsi_agent_store_sqlite::SqliteStore::open(directory.path()).unwrap())
        } else {
            Arc::new(MemoryStore::new())
        };
        let fixture = Fixture::start(store, false).await;
        let before = fixture
            .store
            .read_facts(&fixture.session_id, 0, 16)
            .await
            .unwrap();
        let input = fixture.invocation("toggle-on", true).await;
        let receipt = fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                input.clone(),
            )
            .await
            .unwrap();
        let retry = fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                input.clone(),
            )
            .await
            .unwrap();
        assert_eq!(receipt, retry);
        assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .store
                .read_facts(&fixture.session_id, 0, 16)
                .await
                .unwrap()
                .facts,
            before.facts
        );
        assert_eq!(
            fixture
                .kernel
                .domain_states(&fixture.session_id)
                .await
                .unwrap()[0]
                .snapshot
                .state()
                .value(),
            &serde_json::json!(true)
        );
        let mut different = input.clone();
        different.arguments = CommandArguments::new(false.into()).unwrap();
        assert!(matches!(
            fixture
                .kernel
                .execute(
                    fixture
                        .kernel
                        .prepare_resume(&fixture.session_id)
                        .await
                        .unwrap(),
                    different
                )
                .await,
            Err(TurnError::DomainRequestConflict { .. })
        ));
        assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
        fixture.kernel.shutdown(fixture.workers).await.unwrap();
        let cold = AgentKernel::recover(fixture.store.clone(), fixture.composition.clone())
            .await
            .unwrap();
        let workers = cold.start_workers();
        assert_eq!(
            cold.query(&fixture.session_id, &input.request_id)
                .await
                .unwrap(),
            Some(receipt)
        );
        assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
        cold.shutdown(workers).await.unwrap();
        assert!(fixture.runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn identical_waiters_share_a_callback_and_disconnect_does_not_abandon_its_commit() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), true).await;
    let input = fixture.invocation("shared", true).await;
    let first = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let session = kernel.prepare_resume(&fixture.session_id).await.unwrap();
        let input = input.clone();
        async move { kernel.execute(session, input).await }
    });
    fixture.callback.entered.acquire().await.unwrap().forget();
    let second = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let session = kernel.prepare_resume(&fixture.session_id).await.unwrap();
        let input = input.clone();
        async move { kernel.execute(session, input).await }
    });
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    fixture.callback.release.add_permits(1);
    let receipt = second.await.unwrap().unwrap();
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .kernel
            .query(&fixture.session_id, &input.request_id)
            .await
            .unwrap(),
        Some(receipt)
    );
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn command_deadline_releases_admission_and_publishes_no_replacements() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), true).await;
    let input = fixture.invocation("deadline", true).await;
    let task = tokio::spawn({
        let kernel = fixture.kernel.clone();
        let session = kernel.prepare_resume(&fixture.session_id).await.unwrap();
        let input = input.clone();
        async move { kernel.execute(session, input).await }
    });
    fixture.callback.entered.acquire().await.unwrap().forget();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    assert!(
        matches!(task.await.unwrap(), Err(TurnError::Invalid(message)) if message.contains("deadline"))
    );
    assert!(
        fixture
            .kernel
            .query(&fixture.session_id, &input.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .kernel
            .domain_states(&fixture.session_id)
            .await
            .unwrap()[0]
            .snapshot
            .state()
            .value(),
        &serde_json::json!(false)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn concurrent_callbacks_hold_no_submission_lock_and_loser_does_not_retry() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), true).await;
    let mut tasks = Vec::new();
    for id in ["first", "second"] {
        let input = fixture.invocation(id, true).await;
        let kernel = fixture.kernel.clone();
        let session = kernel.prepare_resume(&fixture.session_id).await.unwrap();
        tasks.push(tokio::spawn(
            async move { kernel.execute(session, input).await },
        ));
        fixture.callback.entered.acquire().await.unwrap().forget();
    }
    fixture.callback.release.add_permits(1);
    tasks.remove(0).await.unwrap().unwrap();
    fixture.callback.release.add_permits(1);
    assert!(matches!(
        tasks.remove(0).await.unwrap(),
        Err(TurnError::CommandRevisionConflict { .. })
    ));
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 2);
    assert!(
        fixture
            .kernel
            .query(
                &fixture.session_id,
                &DomainRequestId::new("second").unwrap()
            )
            .await
            .unwrap()
            .is_none()
    );
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn command_capacity_precedes_callback_and_shutdown_cancels_precommit_work() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), true).await;
    let mut tasks = Vec::new();
    for index in 0..64 {
        let input = fixture.invocation(&format!("request-{index}"), true).await;
        let kernel = fixture.kernel.clone();
        let session = kernel.prepare_resume(&fixture.session_id).await.unwrap();
        tasks.push(tokio::spawn(
            async move { kernel.execute(session, input).await },
        ));
        fixture.callback.entered.acquire().await.unwrap().forget();
    }
    let input = fixture.invocation("overflow", true).await;
    assert!(matches!(
        fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                input
            )
            .await,
        Err(TurnError::Capacity)
    ));
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 64);
    fixture.kernel.shutdown(fixture.workers).await.unwrap();
    for task in tasks {
        assert!(matches!(task.await.unwrap(), Err(TurnError::ShuttingDown)));
    }
    assert_eq!(
        fixture
            .store
            .read_domain_states(&fixture.session_id, None)
            .await
            .unwrap()
            .states[0]
            .snapshot
            .state()
            .value(),
        &serde_json::json!(false)
    );
    assert!(fixture.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn command_admission_rejects_another_kernels_resume_authority() {
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let other = AgentKernel::recover(Arc::new(MemoryStore::new()), fixture.composition.clone())
        .await
        .unwrap();
    let workers = other.start_workers();
    let input = fixture.invocation("foreign", true).await;
    assert!(
        matches!(other.execute(fixture.kernel.prepare_resume(&fixture.session_id).await.unwrap(), input).await, Err(TurnError::Invalid(message)) if message.contains("different Turn service"))
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 0);
    other.shutdown(workers).await.unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn lost_commit_acknowledgements_reconcile_or_remain_queryable_without_callback_replay() {
    for unavailable in [false, true] {
        let store = Arc::new(FactReadRaceStore::new(Arc::new(MemoryStore::new())));
        let fixture = Fixture::start(store.clone(), false).await;
        let input = fixture.invocation("lost-ack", true).await;
        store.fail_domain_after_apply.store(true, Ordering::Release);
        store
            .fail_domain_lookup_after_apply
            .store(unavailable, Ordering::Release);
        let result = fixture
            .kernel
            .execute(
                fixture
                    .kernel
                    .prepare_resume(&fixture.session_id)
                    .await
                    .unwrap(),
                input.clone(),
            )
            .await;
        if unavailable {
            assert!(matches!(
                result,
                Err(TurnError::DomainOutcomeUnknown { .. })
            ));
            store.domain_lookup_fails.store(false, Ordering::Release);
        } else {
            assert!(result.is_ok());
        }
        let receipt = fixture
            .kernel
            .query(&fixture.session_id, &input.request_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fixture
                .kernel
                .execute(
                    fixture
                        .kernel
                        .prepare_resume(&fixture.session_id)
                        .await
                        .unwrap(),
                    input
                )
                .await
                .unwrap(),
            receipt
        );
        assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn draft_commands_bind_the_actual_first_baseline_and_reject_cross_draft_or_stale_results() {
    use rsi_agent_composition_protocol::{DraftCommandError, DraftCommandPreparation};
    let fixture = Fixture::start(Arc::new(MemoryStore::new()), false).await;
    let mut draft =
        AgentSessionDraft::new(header("unpublished-command"), fixture.composition.clone())
            .await
            .unwrap();
    let input = SessionCommandInvocation {
        command: draft.command_descriptors()[0].id().clone(),
        request_id: DomainRequestId::new("draft-toggle").unwrap(),
        expected_revision: draft.revision(),
        arguments: CommandArguments::new(true.into()).unwrap(),
    };
    let before = draft.freeze();
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(input.clone()).unwrap()
    else {
        panic!("new invocation")
    };
    let mutation = prepared.execute(CancellationToken::new()).await.unwrap();
    let receipt = draft.apply_command(mutation).unwrap();
    assert_eq!(receipt.state_sha256(), draft.baseline().digest());
    assert_ne!(receipt.state_sha256(), before.baseline().digest());
    let DraftCommandPreparation::Completed(retry) = draft.prepare_command(input).unwrap() else {
        panic!("exact retry")
    };
    assert_eq!(retry, receipt);
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    let frozen = draft.freeze();
    assert!(matches!(
        fixture.store.header(frozen.header().session_id()).await,
        Err(StoreError::NotFound(_))
    ));
    fixture
        .kernel
        .submit(SubmitTurn {
            session: SubmitSession::Fresh(frozen),
            turn_id: TurnId::new("first-draft-turn").unwrap(),
            text: "use actual initial state".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .kernel
            .domain_states(&SessionId::new("unpublished-command").unwrap())
            .await
            .unwrap()[0]
            .snapshot
            .state()
            .value(),
        &serde_json::json!(true)
    );
    let next = SessionCommandInvocation {
        command: receipt.command().clone(),
        request_id: DomainRequestId::new("next").unwrap(),
        expected_revision: draft.revision(),
        arguments: CommandArguments::new(false.into()).unwrap(),
    };
    let mut other = AgentSessionDraft::new(header("other-draft"), fixture.composition.clone())
        .await
        .unwrap();
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(next.clone()).unwrap()
    else {
        panic!("new invocation")
    };
    assert!(matches!(
        other.apply_command(prepared.execute(CancellationToken::new()).await.unwrap()),
        Err(DraftCommandError::WrongDraft)
    ));
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(next).unwrap() else {
        panic!("new invocation")
    };
    let mutation = prepared.execute(CancellationToken::new()).await.unwrap();
    draft
        .select_preset(AgentPresetId::new("test-agent").unwrap())
        .await
        .unwrap();
    assert!(matches!(
        draft.apply_command(mutation),
        Err(DraftCommandError::Revision { .. })
    ));
    assert_eq!(draft.baseline().digest(), before.baseline().digest());
    assert_eq!(draft.command_receipt(receipt.request_id()), Some(receipt));
    fixture.stop().await;
}

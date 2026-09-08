use super::*;

#[path = "commands/preset.rs"]
mod preset;
#[path = "commands/projections.rs"]
mod projections;
use rsi_agent_composition_protocol::{
    ContributionCatalog, ContributionKind, ContributionRegistration, ContributionResult,
    DomainCatalog, DomainDefinition, DomainHandle, SessionCommand, SessionCommandContext,
    SessionCommandRegistration, ValidatedDomainProposal,
};
use rsi_agent_kernel::AgentKernel;
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, ContributionId, DomainIdentity, DomainRequestId,
    SessionCommandDescriptor, SessionCommandInvocation,
};

#[derive(Debug)]
struct Toggle {
    handle: DomainHandle<bool>,
    calls: AtomicUsize,
    entered: Semaphore,
    release: Semaphore,
    view_gate: std::sync::Mutex<Option<Arc<projections::Gate>>>,
}

#[async_trait]
impl rsi_agent_composition_protocol::SessionProjection for Toggle {
    async fn project(
        &self,
        context: &rsi_agent_composition_protocol::SessionProjectionContext,
        token: CancellationToken,
    ) -> ContributionResult<rsi_agent_session_protocol::ProjectionValue> {
        let gate = self.view_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.tokens.lock().unwrap().push(token);
            gate.entered.add_permits(1);
            gate.release.acquire().await.unwrap().forget();
        }
        let value = self.handle.decode(&context.domains()[0].snapshot).unwrap();
        rsi_agent_session_protocol::ProjectionValue::new(value.into()).map_err(|error| {
            rsi_agent_composition_protocol::ContributionError::Invalid(error.to_string())
        })
    }
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
        self.release.acquire().await.unwrap().forget();
        Ok(vec![
            self.handle
                .propose(
                    context.domains[0].revision,
                    &arguments.value().as_bool().unwrap(),
                )
                .unwrap(),
        ])
    }
}

#[derive(Debug)]
struct Composition {
    pin: AgentCompositionPin,
    entered: Semaphore,
    release: Semaphore,
}
#[async_trait]
impl AgentComposition for Composition {
    async fn default_preset_id(&self) -> Result<AgentPresetId, AgentCompositionError> {
        Ok(self.pin.preset_id().clone())
    }
    async fn pin(
        &self,
        preset: &AgentPresetId,
    ) -> Result<AgentCompositionPin, AgentCompositionError> {
        if preset.as_str() == "missing" {
            return Err(AgentCompositionError::InvalidInput("missing preset".into()));
        }
        if preset.as_str() == "blocked" {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        AgentCompositionPin::new(
            preset.clone(),
            self.pin.source_digest(),
            self.pin.tools(),
            self.pin.context_builder(),
            self.pin.domains().clone(),
            self.pin.contributions().clone(),
            Arc::new(()),
        )
    }
}

struct Fixture {
    runtime: rsi_meta::Runtime,
    service: Arc<LocalSessionService>,
    store: Arc<MemoryStore>,
    kernel: AgentKernel,
    workers: rsi_agent_kernel::KernelWorkers,
    callback: Arc<Toggle>,
    composition: Arc<Composition>,
    _directory: tempfile::TempDir,
    _lease: rsi_meta::RegistrationLease,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let runtime = rsi_meta::Runtime::default();
        let (_, context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
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
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            view_gate: std::sync::Mutex::new(None),
        });
        let id = ContributionId::new("fixture.toggle").unwrap();
        let contributions = ContributionCatalog::freeze(vec![
            (
                ContributionRegistration::new(
                    id.clone(),
                    0,
                    ContributionKind::Command(SessionCommandRegistration::new(
                        SessionCommandDescriptor::new(id, "toggle", "Toggle draft state", true)
                            .unwrap(),
                        callback.clone(),
                    )),
                ),
                position.clone(),
            ),
            (
                ContributionRegistration::new(
                    ContributionId::new("fixture.view").unwrap(),
                    0,
                    ContributionKind::Projection(callback.clone()),
                ),
                position,
            ),
        ])
        .unwrap();
        let composition = Arc::new(Composition {
            pin: AgentCompositionPin::new(
                AgentPresetId::new("fixture").unwrap(),
                "a".repeat(64),
                Arc::new(EmptyTools),
                Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
                domains,
                contributions,
                Arc::new(()),
            )
            .unwrap(),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        let store = Arc::new(MemoryStore::new());
        let kernel = AgentKernel::recover(store.clone(), composition.clone())
            .await
            .unwrap();
        let workers = kernel.start_workers();
        let service = Arc::new(LocalSessionService::new(
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            Arc::new(kernel.clone()),
            Arc::new(kernel.clone()),
            Arc::new(kernel.clone()),
            store.clone(),
            composition.clone(),
            Arc::new(AvailableWorkspace::at(directory.path())),
            Arc::new(TextSettings),
            Arc::new(AvailableLanguage),
            Arc::new(UnavailableImage),
            Arc::new(UnavailableMedia),
            Arc::new(NoApprovalControl),
        ));
        Self {
            runtime,
            service,
            store,
            kernel,
            workers,
            callback,
            composition,
            _directory: directory,
            _lease: lease,
        }
    }
    fn request(id: &str) -> CreateSession {
        CreateSession {
            workspace_id: workspace_id(),
            session_id: SessionId::new(id).unwrap(),
            agent_preset_id: None,
            workspace_trust: WorkspaceTrust::Untrusted,
        }
    }
    async fn create(&self, id: &str) -> Arc<dyn SessionHandle> {
        self.service.create(Self::request(id)).await.unwrap()
    }
    async fn invoke(
        &self,
        handle: &Arc<dyn SessionHandle>,
        request: &str,
        value: bool,
    ) -> SessionCommandInvocation {
        let view = handle.commands().await.unwrap();
        assert_eq!(view.commands().len(), 1);
        SessionCommandInvocation {
            command: view.commands()[0].id().clone(),
            request_id: DomainRequestId::new(request).unwrap(),
            expected_revision: view.revision(),
            arguments: CommandArguments::new(value.into()).unwrap(),
        }
    }
    async fn stop(self) {
        self.service.stop().await;
        self.kernel.shutdown(self.workers).await.unwrap();
        assert!(self.runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn draft_reconnect_retains_receipt_and_first_submit_freezes_the_changed_state() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("draft-command").await;
    let invocation = fixture.invoke(&handle, "enable", true).await;
    fixture.callback.release.add_permits(1);
    let receipt = handle.execute_command(invocation.clone()).await.unwrap();
    assert_eq!(receipt.revision(), CommandRevision::Draft { revision: 1 });
    assert!(
        fixture
            .store
            .header(&SessionId::new("draft-command").unwrap())
            .await
            .is_err()
    );
    let reconnected = fixture
        .service
        .attach(&SessionId::new("draft-command").unwrap())
        .await
        .unwrap();
    assert_eq!(
        reconnected
            .command_status(&invocation.request_id)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        fixture
            .service
            .create(Fixture::request("draft-command"))
            .await
            .unwrap()
            .execute_command(invocation.clone())
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    let mut changed = invocation.clone();
    changed.arguments = CommandArguments::new(false.into()).unwrap();
    assert!(matches!(
        handle.execute_command(changed).await,
        Err(SessionError::CommandConflict { .. })
    ));
    submit_text(handle.clone(), "first", "publish")
        .await
        .unwrap();
    let states = fixture
        .store
        .read_domain_states(&SessionId::new("draft-command").unwrap(), Some(1))
        .await
        .unwrap();
    assert_eq!(states.states.len(), 1);
    assert!(
        fixture
            .callback
            .handle
            .decode(&states.states[0].snapshot)
            .unwrap()
    );
    assert!(matches!(
        handle.commands().await.unwrap().revision(),
        CommandRevision::Durable { .. }
    ));
    assert!(matches!(
        handle.execute_command(invocation).await,
        Err(SessionError::CommandRevisionConflict { .. })
    ));
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    let durable = fixture.invoke(&handle, "disable", false).await;
    fixture.callback.release.add_permits(1);
    let receipt = handle.execute_command(durable.clone()).await.unwrap();
    assert!(matches!(
        receipt.revision(),
        CommandRevision::Durable { .. }
    ));
    assert_eq!(
        handle.command_status(&durable.request_id).await.unwrap(),
        Some(receipt)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn concurrent_exact_draft_commands_join_after_the_first_waiter_disconnects() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("disconnect").await;
    let invocation = fixture.invoke(&handle, "enable", true).await;
    let first = tokio::spawn({
        let handle = handle.clone();
        let invocation = invocation.clone();
        async move { handle.execute_command(invocation).await }
    });
    fixture.callback.entered.acquire().await.unwrap().forget();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let mut changed = invocation.clone();
    changed.arguments = CommandArguments::new(false.into()).unwrap();
    assert!(matches!(
        handle.execute_command(changed).await,
        Err(SessionError::CommandConflict { .. })
    ));
    let retry = tokio::spawn({
        let handle = handle.clone();
        let invocation = invocation.clone();
        async move { handle.execute_command(invocation).await }
    });
    fixture.callback.release.add_permits(1);
    let receipt = retry.await.unwrap().unwrap();
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        handle.command_status(&invocation.request_id).await.unwrap(),
        Some(receipt)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn concurrent_distinct_draft_commands_use_one_revision_without_callback_retry() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("revision-race").await;
    let mut pending = Vec::new();
    for id in ["first", "second"] {
        let invocation = fixture.invoke(&handle, id, true).await;
        let handle = handle.clone();
        pending.push(tokio::spawn(async move {
            handle.execute_command(invocation).await
        }));
        fixture.callback.entered.acquire().await.unwrap().forget();
    }
    fixture.callback.release.add_permits(2);
    let outcomes = futures_util::future::join_all(pending).await;
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Ok(Ok(_))))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                Ok(Err(SessionError::CommandRevisionConflict { .. }))
            ))
            .count(),
        1
    );
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        handle.commands().await.unwrap().revision(),
        CommandRevision::Draft { revision: 1 }
    );
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn draft_command_capacity_precedes_callback_and_stop_drains_all_owned_requests() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("capacity").await;
    let mut pending = Vec::new();
    for index in 0..64 {
        let invocation = fixture
            .invoke(&handle, &format!("request-{index}"), true)
            .await;
        let handle = handle.clone();
        pending.push(tokio::spawn(async move {
            handle.execute_command(invocation).await
        }));
        fixture.callback.entered.acquire().await.unwrap().forget();
    }
    let invocation = fixture.invoke(&handle, "overflow", true).await;
    assert!(matches!(
        handle.execute_command(invocation).await,
        Err(SessionError::Capacity)
    ));
    assert_eq!(fixture.callback.calls.load(Ordering::SeqCst), 64);
    fixture.service.stop().await;
    for request in pending {
        assert!(matches!(
            request.await.unwrap(),
            Err(SessionError::ShuttingDown)
        ));
    }
    fixture.stop().await;
}

#[tokio::test]
async fn first_submission_prevents_a_late_draft_callback_from_replacing_its_baseline() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("publication-race").await;
    let invocation = fixture.invoke(&handle, "enable", true).await;
    let command = tokio::spawn({
        let handle = handle.clone();
        async move { handle.execute_command(invocation).await }
    });
    fixture.callback.entered.acquire().await.unwrap().forget();
    submit_text(handle.clone(), "first", "publish")
        .await
        .unwrap();
    fixture.callback.release.add_permits(1);
    assert!(matches!(
        command.await.unwrap(),
        Err(SessionError::NotFound(_))
    ));
    let states = fixture
        .store
        .read_domain_states(&SessionId::new("publication-race").unwrap(), None)
        .await
        .unwrap();
    assert!(
        !fixture
            .callback
            .handle
            .decode(&states.states[0].snapshot)
            .unwrap()
    );
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn timeout_and_expiry_leave_no_receipt_and_retirement_cancels_owned_callbacks() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("timeout").await;
    let invocation = fixture.invoke(&handle, "enable", true).await;
    let command = tokio::spawn({
        let handle = handle.clone();
        let invocation = invocation.clone();
        async move { handle.execute_command(invocation).await }
    });
    fixture.callback.entered.acquire().await.unwrap().forget();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    assert!(matches!(
        command.await.unwrap(),
        Err(SessionError::Invalid(_))
    ));
    assert!(
        handle
            .command_status(&invocation.request_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        handle.commands().await.unwrap().revision(),
        CommandRevision::Draft { revision: 0 }
    );
    tokio::time::advance(std::time::Duration::from_mins(61)).await;
    assert!(matches!(
        handle.command_status(&invocation.request_id).await,
        Err(SessionError::NotFound(_))
    ));
    let renewed = fixture.create("timeout").await;
    assert!(
        renewed
            .command_status(&invocation.request_id)
            .await
            .unwrap()
            .is_none()
    );
    let command = tokio::spawn({
        let handle = renewed.clone();
        async move { handle.execute_command(invocation).await }
    });
    fixture.callback.entered.acquire().await.unwrap().forget();
    fixture.service.stop().await;
    assert!(matches!(
        command.await.unwrap(),
        Err(SessionError::ShuttingDown)
    ));
    assert!(matches!(
        renewed.commands().await,
        Err(SessionError::NotFound(_) | SessionError::ShuttingDown)
    ));
    fixture.stop().await;
}

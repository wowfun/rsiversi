use async_trait::async_trait;
mod commands;
mod messages;
pub use commands::exact_command_reconciliation;
mod projections;
mod reads;
mod source_reads;
use futures_util::StreamExt;
pub use messages::message_claim_cancellation_and_terminal_delivery;
pub use projections::independent_projection_observation;
pub use reads::bounded_read_capacity_recovery_and_cancellation;
use rsi_agent_session_protocol::{
    MessageId, SessionFact, SessionFactBody, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_turn_protocol::{
    MessageReceipt, MessageState, ObservationCursor, SessionObservation,
};
use rsi_client::{
    ObservationFailure, ObservationSink, ObservationSinkContract, SessionControllerContract,
    SessionControllerFactory,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, Execution, FiberState, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use rsi_session_protocol::{
    InteractionSnapshot, SessionContract, SessionError, SessionHandle, SessionService, SubmitInput,
};
pub use source_reads::owned_source_window_reads;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Semaphore;

fn missing<T>() -> rsi_session_protocol::Result<T> {
    Err(SessionError::NotFound("unused fixture operation".into()))
}

#[derive(Debug)]
struct Active(Arc<AtomicUsize>);
impl Active {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub struct Handle {
    pub commands: commands::Scenario,
    pub source_reads: source_reads::Scenario,
    pub projections: projections::Scenario,
    pub id: SessionId,
    pub submissions: Mutex<Vec<SubmitInput>>,
    pub release: Semaphore,
    pub active_mutations: Arc<AtomicUsize>,
    pub active_streams: Arc<AtomicUsize>,
    pub cursors: Mutex<Vec<ObservationCursor>>,
    pub truncate: bool,
    pub message: Option<messages::Scenario>,
}
impl Handle {
    pub fn new(id: &str, truncate: bool) -> Arc<Self> {
        Arc::new(Self {
            commands: commands::Scenario::default(),
            source_reads: source_reads::Scenario::default(),
            projections: projections::Scenario::default(),
            id: SessionId::new(id).unwrap(),
            submissions: Mutex::new(Vec::new()),
            release: Semaphore::new(0),
            active_mutations: Arc::default(),
            active_streams: Arc::default(),
            cursors: Mutex::new(Vec::new()),
            truncate,
            message: None,
        })
    }
}

#[async_trait]
impl SessionHandle for Handle {
    async fn draft_snapshot(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        panic!("unexpected draft snapshot")
    }
    async fn select_preset(
        &self,
        _: rsi_session_protocol::SelectDraftPreset,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        panic!("unexpected preset selection")
    }

    async fn commands(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandsView> {
        Ok(commands::Scenario::discover())
    }
    async fn execute_command(
        &self,
        invocation: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandReceipt> {
        self.commands.execute(invocation).await
    }
    async fn command_status(
        &self,
        id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> rsi_session_protocol::Result<Option<rsi_agent_session_protocol::SessionCommandReceipt>>
    {
        self.commands.status(id)
    }

    async fn read_message(
        &self,
        _: &MessageId,
        _: u64,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::AgentMessage> {
        missing()
    }
    async fn header(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionHeader> {
        missing()
    }
    async fn submit(&self, request: SubmitInput) -> rsi_session_protocol::Result<MessageReceipt> {
        let _active = Active::new(&self.active_mutations);
        self.submissions.lock().unwrap().push(request.clone());
        if self.message.is_none() {
            self.release.acquire().await.unwrap().forget();
        }
        Ok(MessageReceipt {
            session_id: self.id.clone(),
            message_id: request.message_id,
            accepted_control_seq: 1,
            observed_fact_seq: 0,
            state: self
                .message
                .as_ref()
                .map_or(MessageState::Pending, |case| case.state.clone()),
        })
    }
    async fn message_status(&self, _: &MessageId) -> rsi_session_protocol::Result<MessageReceipt> {
        missing()
    }
    async fn generate_image(
        &self,
        _: rsi_session_protocol::SubmitDirectImage,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::TurnReceipt> {
        missing()
    }
    async fn cancel(
        &self,
        target: rsi_agent_turn_protocol::CancelTarget,
        _: Option<String>,
    ) -> rsi_session_protocol::Result<rsi_agent_turn_protocol::CancelResult> {
        let Some(case) = &self.message else {
            return missing();
        };
        let accepted = matches!(target, rsi_agent_turn_protocol::CancelTarget::Turn(_))
            || case.message_cancel_accept;
        case.cancellations.lock().unwrap().push(target);
        Ok(rsi_agent_turn_protocol::CancelResult {
            accepted,
            already_terminal: false,
        })
    }
    async fn history_before(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionHistoryPage> {
        self.source_reads.read(before, limit).await
    }
    async fn inspect(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_store_protocol::StoreSessionInspection> {
        missing()
    }
    async fn pending_questions(
        &self,
    ) -> rsi_session_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
        missing()
    }
    async fn answer_question(
        &self,
        _: &str,
        _: rsi_user_questions_protocol::QuestionAnswer,
    ) -> rsi_session_protocol::Result<bool> {
        missing()
    }
    async fn pending_approvals(
        &self,
    ) -> rsi_session_protocol::Result<Vec<rsi_approval_protocol::ApprovalRequest>> {
        missing()
    }
    async fn answer_approval(
        &self,
        _: &SessionId,
        _: &str,
        _: rsi_approval_protocol::ApprovalDecision,
    ) -> rsi_session_protocol::Result<bool> {
        missing()
    }
    async fn observe(
        &self,
        cursor: ObservationCursor,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionObservationStream> {
        self.cursors.lock().unwrap().push(cursor);
        let active = Active::new(&self.active_streams);
        if let Some(case) = &self.message {
            return Ok(Box::pin(
                futures_util::stream::iter(case.observations(cursor)).inspect(move |_| {
                    let _ = &active;
                }),
            ));
        }
        let fact = rsi_agent_turn_protocol::ObservationRetention::default()
            .retain_fact(Arc::new(
                SessionFact::new(
                    2,
                    1,
                    SessionFactBody::TurnTerminal {
                        turn_id: TurnId::new("turn").unwrap(),
                        outcome: TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ))
            .unwrap();
        let item = (cursor.fact_seq < 2).then_some(Ok(SessionObservation::Fact {
            fact,
            durable_fact_seq: 100,
        }));
        let stream = futures_util::stream::iter(item);
        if self.truncate && cursor.fact_seq < 2 {
            Ok(Box::pin(stream.inspect(move |_| {
                let _ = &active;
            })))
        } else {
            Ok(Box::pin(
                stream
                    .chain(futures_util::stream::pending())
                    .inspect(move |_| {
                        let _ = &active;
                    }),
            ))
        }
    }
    async fn observe_projections(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::ProjectionStream> {
        self.projections.open(&self.id)
    }
    async fn observe_interactions(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::InteractionStream> {
        let active = Active::new(&self.active_streams);
        let snapshot =
            rsi_session_protocol::InteractionRetention::default().retain(Vec::new(), Vec::new())?;
        Ok(Box::pin(
            futures_util::stream::iter([Ok(snapshot)])
                .chain(futures_util::stream::pending())
                .inspect(move |_| {
                    let _ = &active;
                }),
        ))
    }
}

#[derive(Debug)]
pub struct Service(pub Vec<Arc<Handle>>);
#[async_trait]
impl SessionService for Service {
    async fn create(
        &self,
        _: rsi_session_protocol::CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        missing()
    }
    async fn attach(&self, id: &SessionId) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        self.0
            .iter()
            .find(|handle| &handle.id == id)
            .cloned()
            .map(|handle| handle as Arc<dyn SessionHandle>)
            .ok_or_else(|| SessionError::NotFound(id.to_string()))
    }
    async fn list_recent(
        &self,
        _: Option<&rsi_session_protocol::RecentSessionCursor>,
        _: usize,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::RecentSessionPage> {
        missing()
    }
}
#[async_trait]
impl PluginFactory for Service {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<SessionContract>(Arc::new(Self(self.0.clone())))?;
        plan.defer(
            "withdraw domain",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
pub struct Sink {
    pub observations: AtomicUsize,
    pub interactions: AtomicUsize,
    pub projections: Mutex<Option<rsi_session_protocol::ProjectionSnapshot>>,
    pub projection_stopped: AtomicUsize,
    pub retries: AtomicUsize,
    pub stopped: AtomicUsize,
    pub delivery: Option<Semaphore>,
}
impl Sink {
    pub fn new(blocked: bool) -> Arc<Self> {
        Arc::new(Self {
            observations: AtomicUsize::new(0),
            interactions: AtomicUsize::new(0),
            projections: Mutex::new(None),
            projection_stopped: AtomicUsize::new(0),
            retries: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            delivery: blocked.then(|| Semaphore::new(0)),
        })
    }
}
#[async_trait]
impl ObservationSink for Sink {
    async fn observation(&self, _: SessionObservation) -> Result<(), ObservationFailure> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        if let Some(delivery) = &self.delivery {
            delivery
                .acquire()
                .await
                .map_err(|_| ObservationFailure::SinkStopped)?
                .forget();
        }
        Ok(())
    }
    async fn interactions(&self, _: InteractionSnapshot) -> Result<(), ObservationFailure> {
        self.interactions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn projections(
        &self,
        snapshot: rsi_session_protocol::ProjectionSnapshot,
    ) -> Result<(), ObservationFailure> {
        *self.projections.lock().unwrap() = Some(snapshot);
        Ok(())
    }
    async fn reconnecting(
        &self,
        _: rsi_client::ObservationKind,
        _: &ObservationFailure,
    ) -> Result<(), ObservationFailure> {
        self.retries.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn stopped(&self, kind: rsi_client::ObservationKind, _: &ObservationFailure) {
        if kind == rsi_client::ObservationKind::Projections {
            self.projection_stopped.fetch_add(1, Ordering::SeqCst);
        } else {
            self.stopped.fetch_add(1, Ordering::SeqCst);
        }
    }
}
#[derive(Debug)]
struct Renderer(Arc<Sink>);
#[async_trait]
impl PluginFactory for Renderer {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ObservationSinkContract>(self.0.clone())?;
        let sink = self.0.clone();
        plan.defer(
            "withdraw sink",
            Box::new(move || {
                Box::pin(async move {
                    sink.projections.lock().unwrap().take();
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

pub async fn install_service(runtime: &Runtime, handles: Vec<Arc<Handle>>) {
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "service",
                "test",
                UpdateMode::Replayable,
                Arc::new(Service(handles)),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
}
pub async fn controller(
    context: &Context,
    handle: &Handle,
    sink: Arc<Sink>,
    cursor: Option<ObservationCursor>,
) -> rsi_meta::FiberHandle {
    context
        .apply(
            ResolvedFactory::linked(
                "renderer",
                "test",
                UpdateMode::Replayable,
                Arc::new(Renderer(sink)),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let fiber = context
        .apply(
            ResolvedFactory::linked(
                "controller",
                "test",
                UpdateMode::Replayable,
                Arc::new(SessionControllerFactory),
            ),
            serde_json::json!({"session_id":handle.id,"cursor":cursor}),
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    fiber
}
pub fn input(id: &str) -> SubmitInput {
    SubmitInput {
        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        message_id: MessageId::new(id).unwrap(),
        content: vec![rsi_session_protocol::SessionInput::Text {
            text: "shared controller".into(),
        }],
        model: None,
        sandbox: None,
    }
}
pub async fn until(execution: &Execution, condition: impl Fn() -> bool) {
    execution
        .deadline_after(Duration::from_secs(5))
        .timeout(async {
            while !condition() {
                execution.sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("controller condition did not settle");
}

pub async fn isolated_controller_scopes(execution: Execution) {
    let runtime =
        Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution.clone()).unwrap();
    let first = Handle::new("first", false);
    let second = Handle::new("second", false);
    install_service(&runtime, vec![first.clone(), second.clone()]).await;
    let scopes = rsi_meta_scope::ScopeRoot::new(8).unwrap();
    let mut mounted = Vec::new();
    for handle in [&first, &second] {
        let isolated = runtime
            .root()
            .isolate_local_fresh::<ObservationSinkContract>()
            .unwrap()
            .0
            .isolate_local_fresh::<SessionControllerContract>()
            .unwrap()
            .0;
        let scope = scopes.create(&isolated).await.unwrap();
        let context = scope.context().meta();
        let sink = Sink::new(false);
        controller(
            context,
            handle,
            sink.clone(),
            Some(ObservationCursor::default()),
        )
        .await;
        until(&execution, || {
            sink.observations.load(Ordering::SeqCst) == 1
                && sink.interactions.load(Ordering::SeqCst) == 1
        })
        .await;
        mounted.push((
            context.lookup_local::<SessionControllerContract>().unwrap(),
            scope,
        ));
    }
    assert!(
        runtime
            .root()
            .lookup_local::<SessionControllerContract>()
            .is_none()
    );
    let (first_controller, first_scope) = mounted.remove(0);
    assert!(first_scope.dispose().await.is_clean());
    assert_eq!(first.active_streams.load(Ordering::SeqCst), 0);
    assert_eq!(first.projections.active.load(Ordering::SeqCst), 0);
    assert_eq!(first.projections.retention.retained_bytes(), 0);
    assert_eq!(
        first_controller.submit(input("retired")).await.unwrap_err(),
        SessionError::ShuttingDown
    );
    assert_eq!(second.active_streams.load(Ordering::SeqCst), 2);
    let (second_controller, _) = mounted.remove(0);
    second.release.add_permits(1);
    assert_eq!(
        second_controller
            .submit(input("sibling"))
            .await
            .unwrap()
            .session_id,
        second.id
    );
    assert!(runtime.root().lookup_local::<SessionContract>().is_some());
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(second.active_streams.load(Ordering::SeqCst), 0);
    assert_eq!(second.projections.active.load(Ordering::SeqCst), 0);
    assert_eq!(second.projections.retention.retained_bytes(), 0);
}

pub async fn owned_submission_drain(execution: Execution) {
    let runtime =
        Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution.clone()).unwrap();
    let handle = Handle::new("owned", false);
    install_service(&runtime, vec![handle.clone()]).await;
    let fiber = controller(&runtime.root(), &handle, Sink::new(false), None).await;
    let client = runtime
        .root()
        .lookup_local::<SessionControllerContract>()
        .unwrap();
    assert_eq!(handle.active_streams.load(Ordering::SeqCst), 0);
    for index in 0..4 {
        drop(client.submit(input(&format!("message-{index}"))));
    }
    assert_eq!(
        client.submit(input("excess")).await.unwrap_err(),
        SessionError::Capacity
    );
    until(&execution, || {
        handle.active_mutations.load(Ordering::SeqCst) == 4
    })
    .await;
    let mut retiring = execution.spawn(async move { fiber.dispose().await });
    assert!(
        execution
            .deadline_after(Duration::from_millis(10))
            .timeout(&mut retiring)
            .await
            .is_err()
    );
    assert_eq!(
        client.submit(input("closed")).await.unwrap_err(),
        SessionError::ShuttingDown
    );
    handle.release.add_permits(4);
    assert!(retiring.await.unwrap().is_clean());
    assert_eq!(handle.submissions.lock().unwrap().len(), 4);
    assert_eq!(handle.active_mutations.load(Ordering::SeqCst), 0);
    assert_eq!(handle.active_streams.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}

pub async fn explicit_reconciliation_cancellation(execution: Execution) {
    let runtime =
        Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution.clone()).unwrap();
    let handle = Handle::new("interrupted", false);
    install_service(&runtime, vec![handle.clone()]).await;
    let fiber = controller(&runtime.root(), &handle, Sink::new(false), None).await;
    let client = runtime
        .root()
        .lookup_local::<SessionControllerContract>()
        .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let reply = client.submit_cancellable(input("retained-id"), stop.clone());
    until(&execution, || {
        handle.active_mutations.load(Ordering::SeqCst) == 1
    })
    .await;
    stop.cancel();
    assert!(
        matches!(reply.await, Err(SessionError::MessageOutcomeUnknown { session, message })
        if session == "interrupted" && message == "retained-id")
    );
    assert_eq!(handle.submissions.lock().unwrap().len(), 1);
    assert!(
        execution
            .deadline_after(Duration::from_secs(1))
            .timeout(fiber.dispose())
            .await
            .unwrap()
            .is_clean()
    );
    assert_eq!(handle.active_mutations.load(Ordering::SeqCst), 0);
    assert_eq!(handle.active_streams.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}

pub async fn acknowledged_cursor(execution: Execution) {
    let runtime =
        Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution.clone()).unwrap();
    let handle = Handle::new("cursor", true);
    install_service(&runtime, vec![handle.clone()]).await;
    let sink = Sink::new(true);
    let initial = ObservationCursor {
        control_seq: 7,
        fact_seq: 1,
    };
    controller(&runtime.root(), &handle, sink.clone(), Some(initial)).await;
    until(&execution, || sink.observations.load(Ordering::SeqCst) == 1).await;
    execution.sleep(Duration::from_millis(300)).await;
    assert_eq!(handle.cursors.lock().unwrap().as_slice(), &[initial]);
    sink.delivery.as_ref().unwrap().add_permits(1);
    until(&execution, || handle.cursors.lock().unwrap().len() == 2).await;
    assert_eq!(
        handle.cursors.lock().unwrap()[1],
        ObservationCursor {
            control_seq: 7,
            fact_seq: 2
        }
    );
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(handle.active_streams.load(Ordering::SeqCst), 0);
}

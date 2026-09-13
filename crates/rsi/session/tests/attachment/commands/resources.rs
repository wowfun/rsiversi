use super::*;
use futures_util::StreamExt as _;
use rsi_client::{
    ObservationFailure, ObservationKind, ObservationSink, ObservationSinkContract,
    SessionController, SessionControllerContract, SessionControllerFactory,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberHandle, PluginFactory, PreparedActivation, ResolvedFactory,
    UpdateMode,
};
use rsi_session_protocol::{InteractionSnapshot, ProjectionSnapshot, SessionContract};
use std::time::Duration;

#[derive(Debug)]
struct Sink {
    projections: Semaphore,
    observations: Semaphore,
}
#[async_trait]
impl ObservationSink for Sink {
    async fn observation(
        &self,
        _value: rsi_agent_turn_protocol::SessionObservation,
    ) -> Result<(), ObservationFailure> {
        self.observations.add_permits(1);
        // Hold an actual delivery until its controller is retired.
        std::future::pending().await
    }
    async fn interactions(&self, _: InteractionSnapshot) -> Result<(), ObservationFailure> {
        Ok(())
    }
    async fn projections(&self, _: ProjectionSnapshot) -> Result<(), ObservationFailure> {
        self.projections.add_permits(1);
        Ok(())
    }
    async fn reconnecting(
        &self,
        _: ObservationKind,
        _: &ObservationFailure,
    ) -> Result<(), ObservationFailure> {
        Ok(())
    }
    async fn stopped(&self, _: ObservationKind, _: &ObservationFailure) {}
}
#[derive(Debug)]
struct Providers {
    service: Arc<LocalSessionService>,
    sink: Arc<Sink>,
}
#[async_trait]
impl PluginFactory for Providers {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let session = plan
            .context()
            .provide_local::<SessionContract>(self.service.clone())?;
        let sink = plan
            .context()
            .provide_local::<ObservationSinkContract>(self.sink.clone())?;
        plan.defer(
            "withdraw Session and sink",
            Box::new(move || {
                Box::pin(async move {
                    drop((session, sink));
                    Ok(())
                })
            }),
        )
    }
}
fn input(id: &str) -> SubmitInput {
    SubmitInput {
        message_id: MessageId::new(id).unwrap(),
        delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        content: vec![SessionInput::Text {
            text: "resource plateau".into(),
        }],
        model: None,
        sandbox: None,
    }
}
async fn controller(
    fixture: &Fixture,
    id: &SessionId,
    durable: bool,
) -> (FiberHandle, Arc<SessionController>) {
    let context = fixture
        .runtime
        .root()
        .isolate_local_fresh::<SessionControllerContract>()
        .unwrap()
        .0;
    let cursor = durable.then(rsi_agent_turn_protocol::ObservationCursor::default);
    let fiber = context
        .apply(
            ResolvedFactory::linked(
                "controller",
                "test",
                UpdateMode::Replayable,
                Arc::new(SessionControllerFactory),
            ),
            serde_json::json!({"session_id":id,"cursor":cursor}),
        )
        .await
        .unwrap();
    let controller = context.lookup_local::<SessionControllerContract>().unwrap();
    (fiber, controller)
}
async fn plateau(kernel: &AgentKernel, session: usize, tree: usize, projection: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = kernel.observer_snapshot();
            if (
                s.session.current,
                s.tree.current,
                s.projection.current,
                s.turn.current,
                s.total.current,
            ) == (session, tree, projection, 0, session + tree + projection)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("observer plateau: {:?}", kernel.observer_snapshot()));
}
#[tokio::test]
#[allow(clippy::too_many_lines)] // One repeated public lifecycle includes baseline and retained-owner assertions.
async fn real_kernel_controllers_release_observers_with_idle_handles_and_pending_deliveries() {
    let fixture = Fixture::new().await;
    let sink = Arc::new(Sink {
        projections: Semaphore::new(0),
        observations: Semaphore::new(0),
    });
    let provider = fixture
        .runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "providers",
                "test",
                UpdateMode::Replayable,
                Arc::new(Providers {
                    service: fixture.service.clone(),
                    sink: sink.clone(),
                }),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let baseline = fixture.runtime.resource_snapshot();
    let mut idle = Vec::new();
    for cycle in 0..32 {
        let handle = fixture.create(&format!("resource-{cycle}")).await;
        let id = handle.header().await.unwrap().session_id().clone();
        let (first_fiber, first) = controller(&fixture, &id, false).await;
        sink.projections.acquire().await.unwrap().forget();
        plateau(&fixture.kernel, 0, 0, 1).await;
        first
            .submit(input(&format!("publish-{cycle}")))
            .await
            .unwrap();
        sink.observations.acquire().await.unwrap().forget();
        plateau(&fixture.kernel, 1, 1, 1).await;
        let (second_fiber, second) = controller(&fixture, &id, true).await;
        sink.projections.acquire().await.unwrap().forget();
        plateau(&fixture.kernel, 2, 2, 2).await;
        let mut extra_projection = handle.observe_projections().await.unwrap();
        extra_projection.next().await.unwrap().unwrap();
        plateau(&fixture.kernel, 2, 2, 3).await;
        drop(extra_projection);
        second_fiber.dispose().await;
        plateau(&fixture.kernel, 1, 1, 1).await;
        first_fiber.dispose().await;
        plateau(&fixture.kernel, 0, 0, 0).await;
        assert_eq!(
            fixture
                .kernel
                .observer_snapshot()
                .retained_observation_bytes,
            0
        );
        assert_eq!(
            first.submit(input("retired")).await,
            Err(SessionError::ShuttingDown)
        );
        idle.push((handle, first, second));
        // A provider generation replacement must not resurrect any retired controller.
        provider
            .reconfigure(serde_json::json!({"cycle":cycle}))
            .await
            .unwrap();
        plateau(&fixture.kernel, 0, 0, 0).await;
    }
    let final_resources = fixture.runtime.resource_snapshot();
    assert_eq!(
        final_resources.service_calls.current,
        baseline.service_calls.current
    );
    assert_eq!(
        final_resources.buffered_message_bytes.current,
        baseline.buffered_message_bytes.current
    );
    for (actual, expected) in [
        (final_resources.fibers.current, baseline.fibers.current),
        (final_resources.services.current, baseline.services.current),
        (final_resources.effects.current, baseline.effects.current),
        (
            final_resources.effect_transactions.current,
            baseline.effect_transactions.current,
        ),
        (
            final_resources.listeners.current,
            baseline.listeners.current,
        ),
        (
            final_resources.capability_entries.current,
            baseline.capability_entries.current,
        ),
        (
            final_resources.queued_capability_references.current,
            baseline.queued_capability_references.current,
        ),
        (
            final_resources.pending_message_sends.current,
            baseline.pending_message_sends.current,
        ),
    ] {
        assert_eq!(actual, expected);
    }
    assert_eq!(fixture.kernel.observer_snapshot().total.rejected, 0);
    eprintln!(
        "32 controller cycles: {:?}; idle controller handles={}",
        fixture.kernel.observer_snapshot(),
        idle.len() * 2
    );
    fixture.stop().await;
    assert_eq!(idle.len(), 32);
}

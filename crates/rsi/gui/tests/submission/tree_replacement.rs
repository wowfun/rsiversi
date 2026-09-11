use super::*;
use rsi_client::{ObservationFailure, ObservationKind, ObservationSink, ObservationSinkContract};
use serde_json::{Value, json};

#[derive(Debug)]
struct Sink;
#[async_trait]
impl ObservationSink for Sink {
    async fn observation(
        &self,
        _: SessionObservation,
    ) -> std::result::Result<(), ObservationFailure> {
        Ok(())
    }
    async fn interactions(
        &self,
        _: InteractionSnapshot,
    ) -> std::result::Result<(), ObservationFailure> {
        Ok(())
    }
    async fn projections(
        &self,
        _: ProjectionSnapshot,
    ) -> std::result::Result<(), ObservationFailure> {
        Ok(())
    }
    async fn reconnecting(
        &self,
        _: ObservationKind,
        _: &ObservationFailure,
    ) -> std::result::Result<(), ObservationFailure> {
        Ok(())
    }
    async fn stopped(&self, _: ObservationKind, _: &ObservationFailure) {}
}
#[derive(Debug)]
struct SupplySink;
#[async_trait]
impl PluginFactory for SupplySink {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ObservationSinkContract>(Arc::new(Sink))?;
        plan.defer(
            "withdraw fixture sink",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
async fn apply(
    root: &Context,
    name: &str,
    factory: Arc<dyn PluginFactory>,
    config: Value,
) -> FiberHandle {
    let fiber = root
        .apply(
            ResolvedFactory::linked(name, "fixture", UpdateMode::RestartRequired, factory),
            config,
        )
        .await
        .unwrap();
    assert_eq!(fiber.snapshot().state, FiberState::Active);
    fiber
}
#[tokio::test]
async fn replacing_only_tree_reader_fences_actions_and_cancels_admitted_reads() {
    let (runtime, backend, _app) = sources::fixture().await;
    let (context, reader, id) = setup(&runtime, &backend).await;
    let registry = context.lookup_local::<rsi_ui::UiContract>().unwrap();
    let target = context.lookup_local::<rsi_ui::UiTargetContract>().unwrap();
    let surface = registry
        .surfaces(&target)
        .unwrap()
        .into_iter()
        .find(|s| s.title == "Agent tree")
        .unwrap();
    let view = registry.surface(&surface.reference).unwrap();
    let reference = view.actions["read"].clone();
    let value = view
        .view
        .elements
        .iter()
        .find_map(|element| {
            if let rsi_ui::UiElement::Button { value, .. } = element {
                Some(value.clone())
            } else {
                None
            }
        })
        .unwrap();
    let input = rsi_ui::ActionInput {
        value: value.clone(),
        fields: std::collections::BTreeMap::default(),
    };
    assert!(registry.invoke(&reference, input.clone()).await.is_ok());
    backend.facts.lock().unwrap().push(
        SessionFact::new(
            1,
            1,
            SessionFactBody::TurnAccepted {
                turn_id: TurnId::new("turn").unwrap(),
                text: "exact".into(),
                model: None,
                sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                require_approval: false,
            },
        )
        .unwrap(),
    );
    backend
        .block_source
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let pending = tokio::spawn({
        let registry = registry.clone();
        let reference = reference.clone();
        let id = id.clone();
        async move {
            registry.invoke(&reference, rsi_ui::ActionInput {value:json!({"revision":value["revision"],"operation":{"kind":"source","selected":id,"source":{"seq":"1","field":{"kind":"turn_input"}},"start":"0"}}),fields:std::collections::BTreeMap::default()}).await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while backend
            .active_source
            .load(std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(reader.dispose().await.is_clean());
    assert!(pending.await.unwrap().is_err());
    assert_eq!(
        backend
            .active_source
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    apply(
        &context,
        "reader",
        Arc::new(rsi_session_tree_ui::TreeTargetFactory),
        Value::Null,
    )
    .await;
    assert!(
        registry.is_current(&reference),
        "broader UI target is unchanged"
    );
    assert!(matches!(
        registry.invoke(&reference, input).await,
        Err(rsi_ui::UiError::Retired)
    ));
    assert!(backend.cancel.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
}

async fn setup(runtime: &Runtime, backend: &Backend) -> (Context, FiberHandle, SessionId) {
    let context = runtime.root();
    let id = backend.header().await.unwrap().session_id().clone();
    apply(&context, "sink", Arc::new(SupplySink), Value::Null).await;
    apply(
        &context,
        "controller",
        Arc::new(rsi_client::SessionControllerFactory),
        json!({"session_id":id}),
    )
    .await;
    apply(
        &context,
        "target",
        Arc::new(rsi_ui::UiTargetFactory),
        json!("surface"),
    )
    .await;
    apply(
        &context,
        "tree",
        Arc::new(rsi_session_tree_ui::SessionTreeUiFactory),
        Value::Null,
    )
    .await;
    let reader = apply(
        &context,
        "reader",
        Arc::new(rsi_session_tree_ui::TreeTargetFactory),
        Value::Null,
    )
    .await;
    (context, reader, id)
}

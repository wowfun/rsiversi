use super::*;

#[tokio::test]
async fn listener_capacity_and_exact_handles_cover_active_and_loading_entries() {
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_event_listeners: 1,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let owner_context = Arc::new(Mutex::new(None));
    let owner_listener = Arc::new(Mutex::new(None));
    let owner = runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("listener-owner", "1")),
                context: Arc::clone(&owner_context),
                listener: Arc::clone(&owner_listener),
                dispose_during_activation: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let owner_context = owner_context.lock().unwrap().clone().unwrap();
    let owner_listener = owner_listener.lock().unwrap().clone().unwrap();
    assert!(matches!(
        owner_context.on(
            "authority",
            Arc::new(support::NoopHandler),
            EventOptions::default()
        ),
        Err(MetaError::CapacityExhausted {
            resource: "event listeners"
        })
    ));

    assert_eq!(owner_listener.id(), owner_listener.clone().id());

    owner.dispose().await;
    assert!(matches!(
        owner_context
            .dispatch("authority", DispatchMode::Emit, Value::Null)
            .await,
        Err(MetaError::StaleContext { .. })
    ));

    let loading_context = Arc::new(Mutex::new(None));
    let loading_listener = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                spec: FactorySpec::new(FactoryIdentity::builtin("loading-dispose", "1")),
                context: loading_context,
                listener: loading_listener,
                dispose_during_activation: true,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .root()
            .dispatch("authority", DispatchMode::Emit, Value::Null)
            .await
            .unwrap()
            .invoked,
        0
    );
}

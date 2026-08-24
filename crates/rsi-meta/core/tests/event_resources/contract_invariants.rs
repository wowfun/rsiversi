use super::*;

#[tokio::test]
async fn listener_capacity_and_generation_authority_cover_active_and_staged_entries() {
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
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("listener-owner", "1")),
                context: Arc::clone(&owner_context),
                listener: Arc::clone(&owner_listener),
                remove_while_staged: false,
            }),
            Value::Null,
        )
        .await
        .unwrap();
    let owner_context = owner_context.lock().unwrap().clone().unwrap();
    let owner_listener = owner_listener.lock().unwrap().unwrap();
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

    let foreign_context = Arc::new(Mutex::new(None));
    let consumer = runtime
        .root()
        .apply(
            Arc::new(ContextCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("foreign", "1")),
                context: Arc::clone(&foreign_context),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Active));
    assert!(
        !foreign_context
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .off(owner_listener)
    );
    assert!(!runtime.root().off(owner_listener));

    owner.dispose().await;
    assert!(matches!(
        owner_context
            .dispatch("authority", DispatchMode::Emit, Value::Null)
            .await,
        Err(MetaError::StaleContext { .. })
    ));

    let staged_context = Arc::new(Mutex::new(None));
    let staged_listener = Arc::new(Mutex::new(None));
    runtime
        .root()
        .apply(
            Arc::new(ListenerCaptureFactory {
                descriptor: PluginDescriptor::new(FactoryIdentity::builtin("staged-off", "1")),
                context: staged_context,
                listener: staged_listener,
                remove_while_staged: true,
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

use super::*;
use rsi_meta::{InspectedFiberState, InspectionRequest};
use rsi_meta_scope::ScopeRoot;

pub(super) async fn probe(execution: Execution) {
    let runtime = Runtime::with_execution(Default::default(), execution).unwrap();
    let scopes = ScopeRoot::new(2).unwrap();
    let scope = scopes.create(&runtime.root()).await.unwrap();
    let context = scope.context().meta().clone();
    let provider = context
        .apply(
            resolved(
                "inspection-provider",
                Provider(Arc::new(AtomicUsize::new(7))),
            ),
            serde_json::json!({"private_config":"inspection-secret"}),
        )
        .await
        .unwrap();
    let cleanups = Arc::new(AtomicUsize::new(0));
    let consumer = context
        .apply(
            resolved(
                "inspection-consumer",
                Consumer {
                    observed: Arc::new(AtomicUsize::new(0)),
                    cleanups: cleanups.clone(),
                },
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let outside = runtime
        .root()
        .apply(
            resolved(
                "outside-inspection-scope",
                Failing(Arc::new(Mutex::new(Vec::new()))),
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let request = InspectionRequest::default();
    let scoped = context.inspect(request).unwrap();
    assert_eq!(scoped.total_fibers, 3);
    assert!(scoped.resources.is_none());
    assert!(scoped.fibers.iter().all(|row| row.id != outside.id()));
    let row = scoped
        .fibers
        .iter()
        .find(|row| row.id == consumer.id())
        .unwrap();
    assert_eq!(row.state, InspectedFiberState::Active);
    assert_eq!(
        row.dependencies.items[0]
            .provider
            .as_ref()
            .unwrap()
            .owner
            .fiber,
        provider.id()
    );
    assert!(row.retained_effect_entries > 0);
    assert!(!format!("{scoped:?}").contains("inspection-secret"));
    let global = runtime.inspect(request).unwrap();
    assert_eq!(global.total_fibers, 4);
    assert!(global.resources.is_some());
    assert!(!format!("{global:?}").contains("intentional rollback"));
    let first = runtime
        .inspect(InspectionRequest {
            maximum_fibers: 1,
            ..request
        })
        .unwrap();
    assert_eq!(first.fibers.len(), 1);
    let second = runtime
        .inspect(InspectionRequest {
            after: first.next_after,
            maximum_fibers: 1,
            ..request
        })
        .unwrap();
    assert!(first.fibers[0].id < second.fibers[0].id);
    assert!(scope.dispose().await.is_clean());
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
    assert!(context.inspect(request).is_err());
    assert_eq!(runtime.inspect(request).unwrap().total_fibers, 1);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
}

use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, InspectedFiberState, InspectionRequest, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
struct Failed;
#[async_trait]
impl PluginFactory for Failed {
    fn prepare(&self, desired: &Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, _: ActivationPlan) -> rsi_meta::Result<()> {
        Err(rsi_meta::MetaError::Service("secret failure detail".into()))
    }
}

#[tokio::test]
async fn inspection_is_bounded_redacted_and_pages_live_membership() {
    let runtime = Runtime::default();
    let factory = ResolvedFactory::linked(
        "test.inspect",
        "revision",
        UpdateMode::Replayable,
        Arc::new(Failed),
    );
    let first = runtime
        .root()
        .apply(factory.clone(), json!({"secret_config":"do not expose"}))
        .await
        .unwrap();
    let second = runtime.root().apply(factory, Value::Null).await.unwrap();
    let request = InspectionRequest {
        maximum_fibers: 1,
        ..InspectionRequest::default()
    };
    let page = runtime.inspect(request).unwrap();
    assert_eq!(page.total_fibers, 2);
    assert_eq!(page.fibers.len(), 1);
    assert_eq!(page.fibers[0].id, first.id());
    assert_eq!(page.fibers[0].state, InspectedFiberState::Failed);
    assert!(page.resources.is_some());
    assert_eq!(page.next_after, Some(first.id()));
    let next = runtime
        .inspect(InspectionRequest {
            after: page.next_after,
            ..request
        })
        .unwrap();
    assert_eq!(next.fibers[0].id, second.id());
    assert_eq!(next.next_after, None);
    assert!(!format!("{page:?}").contains("secret"));
    assert!(!format!("{page:?}").contains("do not expose"));
    assert!(
        runtime
            .inspect(InspectionRequest {
                maximum_fibers: 0,
                ..request
            })
            .is_err()
    );
    assert!(
        runtime
            .inspect(InspectionRequest {
                maximum_fibers: 65,
                ..request
            })
            .is_err()
    );
    assert!(
        runtime
            .inspect(InspectionRequest {
                maximum_items: 129,
                ..request
            })
            .is_err()
    );
    runtime.mark_terminal("terminal secret detail");
    let terminal = runtime.inspect(request).unwrap();
    assert!(terminal.terminal);
    assert!(!format!("{terminal:?}").contains("secret"));
    assert!(runtime.shutdown().await.is_complete());
}

struct Number;
impl rsi_meta::LocalContract for Number {
    const KEY: &'static str = "inspect.number";
    type Service = usize;
}
#[derive(Debug)]
struct Endpoint;
#[async_trait]
impl rsi_meta::ServiceEndpoint for Endpoint {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        _: rsi_meta::ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
enum Role {
    Provider,
    Consumer,
    Parent,
}
#[derive(Debug)]
struct Node {
    role: Role,
    contexts: Arc<std::sync::Mutex<Vec<rsi_meta::Context>>>,
}
#[async_trait]
impl PluginFactory for Node {
    fn prepare(&self, desired: &Value) -> rsi_meta::Result<PreparedActivation> {
        let prepared = PreparedActivation::new(desired.clone());
        Ok(if matches!(self.role, Role::Consumer) {
            prepared
                .requiring(rsi_meta::Requirement::new(
                    "inspect.echo",
                    "inspect.echo.v1",
                    rsi_meta::ContractVersion(1),
                ))
                .requiring_local::<Number>()
        } else {
            prepared
        })
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        if matches!(self.role, Role::Provider) {
            plan.context().provide_local::<Number>(Arc::new(7))?;
            plan.context().provide(
                "inspect.echo",
                "inspect.echo.v1",
                rsi_meta::ContractVersion(1),
                Arc::new(Endpoint),
            )?;
        }
        plan.defer(
            "secret effect label",
            Box::new(|| Box::pin(async { Ok(()) })),
        )?;
        self.contexts.lock().unwrap().push(plan.context().clone());
        Ok(())
    }
}
fn node(role: Role, contexts: &Arc<std::sync::Mutex<Vec<rsi_meta::Context>>>) -> ResolvedFactory {
    ResolvedFactory::linked(
        "inspect.node",
        "1",
        UpdateMode::Replayable,
        Arc::new(Node {
            role,
            contexts: contexts.clone(),
        }),
    )
}

#[tokio::test]
async fn inspection_reports_exact_bindings_isolation_and_effect_prefixes_without_values() {
    use rsi_meta::{InspectedService, IsolationId, LocalIsolationId};
    let runtime = Runtime::default();
    let contexts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let context = runtime
        .root()
        .isolate("inspect.echo", IsolationId(42))
        .unwrap()
        .isolate_local::<Number>(LocalIsolationId(24))
        .unwrap();
    let consumer = context
        .apply(node(Role::Consumer, &contexts), Value::Null)
        .await
        .unwrap();
    let request = InspectionRequest {
        maximum_items: 1,
        ..InspectionRequest::default()
    };
    let pending = runtime.inspect(request).unwrap().fibers.remove(0);
    assert_eq!(pending.state, InspectedFiberState::Pending);
    assert_eq!(pending.dependencies.total, 2);
    assert_eq!(pending.dependencies.items.len(), 1);
    assert!(pending.dependencies.items[0].provider.is_none());
    assert!(matches!(
        pending.dependencies.items[0].service,
        InspectedService::Portable {
            isolation: IsolationId(42),
            ..
        }
    ));
    let provider = context
        .apply(node(Role::Provider, &contexts), Value::Null)
        .await
        .unwrap();
    consumer
        .wait_active(&tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let page = runtime.inspect(InspectionRequest::default()).unwrap();
    let row = page
        .fibers
        .iter()
        .find(|row| row.id == consumer.id())
        .unwrap();
    assert_eq!(row.dependencies.total, 2);
    for dependency in &row.dependencies.items {
        let binding = dependency.provider.as_ref().unwrap();
        assert_eq!(binding.owner.fiber, provider.id());
        assert_eq!(binding.owner.generation, provider.snapshot().generation);
        assert_ne!(binding.supply_token, 0);
    }
    assert!(matches!(
        row.dependencies.items[1].service,
        InspectedService::Local {
            isolation: LocalIsolationId(24),
            ..
        }
    ));
    let row = page
        .fibers
        .iter()
        .find(|row| row.id == provider.id())
        .unwrap();
    assert_eq!(row.supplies.total, 2);
    assert!(
        row.supplies
            .items
            .iter()
            .all(|supply| supply.generation_published)
    );
    assert!(!format!("{page:?}").contains("secret"));
    let provider_context = contexts
        .lock()
        .unwrap()
        .iter()
        .find(|ctx| ctx.owner().unwrap().0 == provider.id())
        .unwrap()
        .clone();
    assert_effect_views(&provider_context).await;
    assert!(runtime.shutdown().await.is_complete());
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
}

#[tokio::test]
async fn context_inspection_scopes_membership_tracks_order_and_fences_retirement() {
    let runtime = Runtime::default();
    let contexts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(node(Role::Parent, &contexts), Value::Null)
        .await
        .unwrap();
    let scope = contexts.lock().unwrap()[0].clone();
    runtime
        .root()
        .apply(node(Role::Parent, &contexts), Value::Null)
        .await
        .unwrap();
    let first_position = scope.child_position().unwrap();
    let second_position = scope.child_position().unwrap();
    let first = scope
        .with_child_position(&first_position)
        .unwrap()
        .apply(node(Role::Parent, &contexts), Value::Null)
        .await
        .unwrap();
    let second = scope
        .with_child_position(&second_position)
        .unwrap()
        .apply(node(Role::Parent, &contexts), Value::Null)
        .await
        .unwrap();
    let page = scope.inspect(InspectionRequest::default()).unwrap();
    assert_eq!(page.total_fibers, 3);
    assert!(page.resources.is_none());
    assert_eq!(page.fibers[0].children, 2);
    assert!(page.fibers[1].order < page.fibers[2].order);
    assert_eq!(page.fibers[1].parent.unwrap().fiber, parent.id());
    scope
        .reorder_children(&[second_position, first_position])
        .unwrap();
    let reordered = scope.inspect(InspectionRequest::default()).unwrap();
    assert_eq!(reordered.fibers[1].id, first.id());
    assert_eq!(reordered.fibers[2].id, second.id());
    assert!(reordered.fibers[1].order > reordered.fibers[2].order);
    assert_eq!(page.fibers[1].generation, reordered.fibers[1].generation);
    assert!(parent.dispose().await.is_clean());
    assert!(matches!(
        scope.inspect(InspectionRequest::default()),
        Err(rsi_meta::MetaError::StaleContext { .. })
    ));
    assert_eq!(
        runtime
            .inspect(InspectionRequest::default())
            .unwrap()
            .total_fibers,
        1
    );
    assert!(runtime.shutdown().await.is_complete());
}

async fn assert_effect_views(provider_context: &rsi_meta::Context) {
    let mut txn = provider_context
        .begin_effect("secret open transaction")
        .unwrap();
    txn.defer("secret undo", Box::new(|| Box::pin(async { Ok(()) })))
        .unwrap();
    let page = provider_context
        .inspect(InspectionRequest::default())
        .unwrap();
    let open = page.fibers[0]
        .effects
        .items
        .iter()
        .find(|effect| effect.open)
        .unwrap();
    assert_eq!(open.queued_entries, 1);
    assert_eq!(open.cleanup, rsi_meta::InspectedCleanupState::Unclaimed);
    assert_eq!(open.cleanup_failures, None);
    let id = open.id;
    let effect = txn.commit().unwrap();
    let page = provider_context
        .inspect(InspectionRequest::default())
        .unwrap();
    assert!(
        !page.fibers[0]
            .effects
            .items
            .iter()
            .find(|effect| effect.id == id)
            .unwrap()
            .open
    );
    assert!(effect.dispose().await.is_clean());
}

#[tokio::test]
async fn inspection_distinguishes_transferred_cleanup_from_an_empty_effect_budget() {
    let runtime = Runtime::default();
    let contexts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let parent = runtime
        .root()
        .apply(node(Role::Parent, &contexts), Value::Null)
        .await
        .unwrap();
    let context = contexts.lock().unwrap()[0].clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut txn = context.begin_effect("secret held effect").unwrap();
    txn.defer(
        "secret undo diagnostic",
        Box::new({
            let entered = entered.clone();
            let release = release.clone();
            move || {
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Err("secret cleanup refusal".into())
                })
            }
        }),
    )
    .unwrap();
    let disposal = tokio::spawn({
        let parent = parent.clone();
        async move { parent.dispose().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let page = runtime.inspect(InspectionRequest::default()).unwrap();
            if page.fibers[0].cleanup_phase == Some(rsi_meta::CleanupPhase::RunningEffects) {
                assert_eq!(page.fibers[0].state, InspectedFiberState::Unloading);
                assert_eq!(page.fibers[0].effects.total, 0);
                assert!(page.fibers[0].retained_effect_entries > 0);
                assert!(page.fibers[0].retained_effect_transactions > 0);
                assert!(!format!("{page:?}").contains("secret"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(txn);
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let page = runtime.inspect(InspectionRequest::default()).unwrap();
    assert!(page.fibers[0].retained_effect_entries > 0);
    assert!(!disposal.is_finished());
    release.notify_one();
    let report = disposal.await.unwrap();
    assert_eq!(report.total_failures(), 1);
    let page = runtime.inspect(InspectionRequest::default()).unwrap();
    assert!(page.fibers.is_empty());
    assert_eq!(page.resources.unwrap().effects.current, 0);
    assert!(runtime.shutdown().await.is_complete());
}

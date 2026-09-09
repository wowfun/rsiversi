#![deny(unsafe_code)]
#![cfg(target_arch = "wasm32")]

use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Execution, FiberState, LocalContract, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wasm_bindgen::prelude::*;

mod composition;
mod inspection;
#[path = "../../../../crates/rsi-meta/profile/tests/namespaces.rs"]
mod profile_namespace_probe;

struct CounterContract;
impl LocalContract for CounterContract {
    const KEY: &'static str = "probe.counter";
    type Service = AtomicUsize;
}

#[derive(Debug)]
struct Provider(Arc<AtomicUsize>);
#[async_trait]
impl PluginFactory for Provider {
    fn prepare(&self, desired: &serde_json::Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = plan.config().get("value").map_or_else(
            || self.0.clone(),
            |value| {
                Arc::new(AtomicUsize::new(
                    value.as_u64().unwrap().try_into().unwrap(),
                ))
            },
        );
        plan.context().provide_local::<CounterContract>(service)?;
        Ok(())
    }
}

#[derive(Debug)]
struct Consumer {
    observed: Arc<AtomicUsize>,
    cleanups: Arc<AtomicUsize>,
}
#[async_trait]
impl PluginFactory for Consumer {
    fn prepare(&self, desired: &serde_json::Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<CounterContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.observed.store(
            plan.local::<CounterContract>()?.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
        let cleanups = self.cleanups.clone();
        plan.defer(
            "consumer cleanup",
            Box::new(move || {
                Box::pin(async move {
                    cleanups.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct Failing(Arc<Mutex<Vec<u8>>>);
#[async_trait]
impl PluginFactory for Failing {
    fn prepare(&self, desired: &serde_json::Value) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        for index in 0..3 {
            let order = self.0.clone();
            plan.defer(
                "rollback",
                Box::new(move || {
                    Box::pin(async move {
                        order.lock().unwrap().push(index);
                        Ok(())
                    })
                }),
            )?;
        }
        Err(rsi_meta::MetaError::Activation(
            "intentional rollback".into(),
        ))
    }
}

fn resolved(name: &str, factory: impl PluginFactory + 'static) -> ResolvedFactory {
    ResolvedFactory::linked(name, "probe-v1", UpdateMode::Replayable, Arc::new(factory))
}

async fn execution_probe(execution: &Execution) {
    let timers = (0..512)
        .map(|_| execution.deadline_after(Duration::from_secs(60)).wait())
        .collect::<Vec<_>>();
    let snapshot = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(snapshot.pending_timers, 512);
    assert_eq!(snapshot.active_alarms, 1);
    drop(timers);
    assert_eq!(
        rsi_meta_execution::browser_resource_snapshot().pending_timers,
        0
    );
    assert_eq!(
        rsi_meta_execution::browser_resource_snapshot().active_alarms,
        0
    );

    let (sent, received) = tokio::sync::oneshot::channel();
    let release = Arc::new(Notify::new());
    drop(execution.spawn({
        let release = release.clone();
        async move {
            release.notified().await;
            sent.send(17).unwrap();
        }
    }));
    release.notify_one();
    assert_eq!(received.await.unwrap(), 17);
    let (sent, received) = tokio::sync::oneshot::channel();
    drop(execution.prepare(move || sent.send(23).unwrap()));
    assert_eq!(received.await.unwrap(), 23);

    let deadline = execution.deadline_after(Duration::from_millis(5));
    deadline.wait().await;
    assert!(deadline.has_elapsed());
    let late = execution.deadline_after(Duration::from_millis(2));
    let result = late
        .timeout(async {
            while !late.has_elapsed() {
                std::hint::spin_loop();
            }
            31
        })
        .await;
    assert!(result.is_err());
}

async fn lifecycle_probe(execution: Execution) {
    let runtime = Runtime::with_execution(Default::default(), execution).unwrap();
    let observed = Arc::new(AtomicUsize::new(0));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let consumer = runtime
        .root()
        .apply(
            resolved(
                "consumer",
                Consumer {
                    observed: observed.clone(),
                    cleanups: cleanups.clone(),
                },
            ),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(consumer.snapshot().state, FiberState::Pending(_)));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(consumer.wait_active(&cancellation).await.is_err());
    let provider = runtime
        .root()
        .apply(
            resolved("provider", Provider(Arc::new(AtomicUsize::new(41)))),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    consumer
        .wait_active(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(observed.load(Ordering::SeqCst), 41);
    assert!(provider.dispose().await.is_clean());
    let mut changes = consumer.subscribe();
    while !matches!(changes.borrow().state, FiberState::Pending(_)) {
        changes.changed().await.unwrap();
    }
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);

    let order = Arc::new(Mutex::new(Vec::new()));
    let failed = runtime
        .root()
        .apply(
            resolved("failing", Failing(order.clone())),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(failed.snapshot().state, FiberState::Failed(_)));
    assert_eq!(*order.lock().unwrap(), [2, 1, 0]);
    assert!(runtime.shutdown().await.is_clean());
    drop((consumer, provider, failed, changes));
    let s = runtime.resource_snapshot();
    for usage in [
        s.preparations,
        s.fibers,
        s.retained_plugin_bytes,
        s.dependency_edges,
        s.services,
        s.effects,
        s.effect_transactions,
        s.listeners,
        s.capability_entries,
        s.queued_capability_references,
        s.service_calls,
        s.buffered_message_bytes,
        s.pending_message_sends,
        s.reconciliations,
        s.scheduler_workers,
        s.cleanup_runs,
    ] {
        assert_eq!(
            usage.current, 0,
            "resource was retained after shutdown: {usage:?}"
        );
    }
}

#[wasm_bindgen]
pub async fn run_probe() -> Result<String, JsValue> {
    let execution = Execution::browser().map_err(|e| JsValue::from_str(&e.to_string()))?;
    execution_probe(&execution).await;
    lifecycle_probe(execution.clone()).await;
    profile_probe(execution.clone()).await;
    child_profiles_probe(execution.clone()).await;
    composition::probe(execution.clone()).await;
    inspection::probe(execution.clone()).await;
    profile_namespace_probe::namespace_scenario(
        Runtime::with_execution(Default::default(), execution).unwrap(),
    )
    .await;
    let timers = rsi_meta_execution::browser_resource_snapshot();
    assert_eq!(timers.pending_timers, 0);
    assert_eq!(timers.active_alarms, 0);
    Ok(serde_json::json!({
        "status": "passed", "pending_timers": timers.pending_timers,
        "active_alarms": timers.active_alarms,
        "cases": ["named isolation includes", "static namespace independence", "timer cancellation", "detached task", "detached preparation",
            "clock expiry", "non-yielding late result", "activation", "withdrawal",
            "wait cancellation", "rollback order", "shutdown resources",
            "Profile bundle and Rhai", "Profile reload without files",
            "child Profile isolation and disposal", "declaration order and prepend",
            "registration snapshot and selective rebuild", "Scope contribution cleanup",
            "bounded redacted ownership inspection", "scoped inspection retirement fence"]
    })
    .to_string())
}

#[wasm_bindgen]
pub fn trap_probe() {
    panic!("intentional Worker trap; no cleanup acknowledgement is possible");
}

async fn profile_probe(execution: Execution) {
    use rsi_host::{HostBuilder, ProfileBundle, ProfileLimits, ProfileProgram, WatcherHealth};
    use std::collections::BTreeMap;
    let observed = Arc::new(AtomicUsize::new(0));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let mut builder = HostBuilder::without_paths("browser-worker").execution(execution);
    builder.define("count", serde_json::json!(41)).unwrap();
    builder
        .register_local_contract::<CounterContract>()
        .unwrap();
    builder
        .register_linked(
            "provider",
            "v1",
            UpdateMode::Replayable,
            Arc::new(Provider(Arc::new(AtomicUsize::new(0)))),
        )
        .unwrap();
    builder
        .register_linked(
            "consumer",
            "v1",
            UpdateMode::Replayable,
            Arc::new(Consumer {
                observed: observed.clone(),
                cleanups: cleanups.clone(),
            }),
        )
        .unwrap();
    let bundle = ProfileBundle::new("root.toml", BTreeMap::from([
        ("root.toml".into(), b"format = 1\n[[steps]]\nkind = 'include'\npath = 'plugins.toml'\n".to_vec()),
        ("plugins.toml".into(), b"format = 1\n[[steps]]\nkind = 'plugin'\nid = 'provider'\nplugin = 'provider'\nconfig_rhai = '#{ value: defines.count + 1 }'\n[[steps]]\nkind = 'plugin'\nid = 'consumer'\nplugin = 'consumer'\n".to_vec()),
    ]), &ProfileLimits::default()).unwrap();
    let host = builder.build().unwrap();
    assert!(host.paths().is_none());
    let program = ProfileProgram::from_bundle(bundle);
    let preview = host.preview_program(program.clone()).unwrap();
    assert_eq!(preview.leaves.len(), 2);
    let running = host.start_program(program).await.unwrap();
    assert_eq!(observed.load(Ordering::SeqCst), 42);
    assert_eq!(running.profile_status().watcher(), WatcherHealth::Inactive);
    running.reload().await.unwrap();
    assert_eq!(cleanups.load(Ordering::SeqCst), 0);
    assert!(running.shutdown().await.is_clean());
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
}

async fn child_profiles_probe(execution: Execution) {
    use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
    use rsi_meta_scope::ScopeRoot;
    let runtime = Runtime::with_execution(Default::default(), execution).unwrap();
    runtime
        .root()
        .apply(
            resolved("parent", Provider(Arc::new(AtomicUsize::new(99)))),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    let scopes = ScopeRoot::new(4).unwrap();
    let mut children = Vec::new();
    for value in [10, 20] {
        let scope = scopes.create(&runtime.root()).await.unwrap();
        let mut builder = HostBuilder::without_paths("browser-worker");
        builder
            .register_local_contract::<CounterContract>()
            .unwrap();
        builder
            .register_linked(
                "provider",
                "v1",
                UpdateMode::Replayable,
                Arc::new(Provider(Arc::new(AtomicUsize::new(0)))),
            )
            .unwrap();
        let host = builder.build().unwrap();
        let context = host
            .isolate_local_context(scope.context().meta().clone())
            .unwrap();
        let bootstrap = host
            .prepare_in(
                &runtime,
                ProfileProgram::from_profile(Profile::new([ProfileEntry::new(
                    "child",
                    "provider",
                    serde_json::json!({"value": value}),
                )])),
            )
            .await
            .unwrap();
        let control = bootstrap.control();
        let fiber = context
            .apply(
                ResolvedFactory::linked(
                    "profile",
                    "probe-v1",
                    UpdateMode::RestartRequired,
                    bootstrap.factory(),
                ),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, FiberState::Active);
        assert_eq!(
            context
                .lookup_local::<CounterContract>()
                .unwrap()
                .load(Ordering::SeqCst),
            value
        );
        children.push((scope, context, control));
    }
    let (first, first_context, first_control) = children.remove(0);
    assert!(first.dispose().await.is_clean());
    assert!(first_context.lookup_local::<CounterContract>().is_none());
    assert!(first_control.reload().await.is_err());
    let (second, second_context, second_control) = children.remove(0);
    second_control.reload().await.unwrap();
    assert_eq!(
        second_context
            .lookup_local::<CounterContract>()
            .unwrap()
            .load(Ordering::SeqCst),
        20
    );
    assert_eq!(
        runtime
            .root()
            .lookup_local::<CounterContract>()
            .unwrap()
            .load(Ordering::SeqCst),
        99
    );
    assert!(second.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

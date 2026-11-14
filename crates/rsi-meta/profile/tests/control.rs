use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, FiberState, LocalContract, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, Runtime, RuntimeLimits, TopologyLimits, UpdateMode,
};
use rsi_meta_profile::{
    ProfileBootstrap, ProfileControlContract, ProfileEnvironment, ProfileHealth,
    ProfileInstanceState, ProfileLimits, ProfileProgram, ProfileResolver, ReloadOutcome,
    WatcherHealth,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[path = "control/manual_clock.rs"]
mod manual_clock;

enum ProbeContract {}

impl LocalContract for ProbeContract {
    const KEY: &'static str = "test.local";
    type Service = AtomicUsize;
}

struct ProbeEvent;
impl rsi_meta::LocalEvent for ProbeEvent {
    const KEY: &'static str = "test.event";
    type Value = ();
    type Error = std::convert::Infallible;
    type Mode = rsi_meta::Emit;
}
struct OrderEvent;
impl rsi_meta::LocalEvent for OrderEvent {
    const KEY: &'static str = "test.order";
    type Value = Arc<Mutex<Vec<String>>>;
    type Error = std::convert::Infallible;
    type Mode = rsi_meta::Emit;
}
#[derive(Debug)]
struct OrderHandler(String);
impl rsi_meta::EmitEventHandler<OrderEvent> for OrderHandler {
    fn handle(&self, order: &Arc<Mutex<Vec<String>>>) {
        order.lock().unwrap().push(self.0.clone());
    }
}
#[derive(Debug)]
struct CountEvent(Arc<AtomicUsize>);
impl rsi_meta::EmitEventHandler<ProbeEvent> for CountEvent {
    fn handle(&self, (): &()) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Debug)]
struct Echo;
#[async_trait]
impl rsi_meta::ServiceEndpoint for Echo {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: rsi_meta::ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        while let Some(message) = channel.recv().await {
            channel.send(message).await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ProbeFactory {
    starts: Arc<AtomicUsize>,
    fail_once: Arc<AtomicUsize>,
    prepare_calls: Arc<AtomicUsize>,
    cleanup_gate: Option<Arc<CleanupGate>>,
}

#[derive(Debug, Default)]
struct CleanupGate {
    started: Notify,
    release: Notify,
}

#[derive(Debug)]
struct SupplyFactory;

#[async_trait]
impl PluginFactory for SupplyFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let counter = Arc::new(AtomicUsize::new(0));
        let supply = plan
            .context()
            .provide_local::<ProbeContract>(counter.clone())?;
        if plan.config().get("all_lanes").and_then(Value::as_bool) == Some(true) {
            plan.context().on_emit::<ProbeEvent, _>(
                Arc::new(CountEvent(counter)),
                rsi_meta::LocalEventOptions::default(),
            )?;
            plan.context().provide(
                "test.portable",
                "test.echo",
                rsi_meta::ContractVersion(1),
                Arc::new(Echo),
            )?;
        }
        plan.defer(
            "withdraw test Probe service",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[async_trait]
impl PluginFactory for ProbeFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let mode = desired.get("mode").and_then(Value::as_str).unwrap_or("ok");
        let prepare_call = self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        if mode == "prepare-fail" {
            return Err(MetaError::InvalidConfig("candidate rejected".to_owned()));
        }
        if mode == "old-prepare-fails-on-reload" && prepare_call > 0 {
            return Err(MetaError::InvalidConfig(
                "rollback preparation rejected".to_owned(),
            ));
        }
        let prepared = PreparedActivation::new(desired.clone());
        if mode == "bound" || mode == "bound-fail" {
            return Ok(prepared.requiring_local::<ProbeContract>().requiring(
                rsi_meta::Requirement::new(
                    "test.portable",
                    "test.echo",
                    rsi_meta::ContractVersion(1),
                ),
            ));
        }
        if mode == "pending" || mode == "pending-activate-fail" {
            return Ok(prepared.requiring_local::<ProbeContract>());
        }
        Ok(prepared)
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        if let Some(id) = plan.config().get("order").and_then(Value::as_str) {
            plan.context().on_emit::<OrderEvent, _>(
                Arc::new(OrderHandler(id.into())),
                rsi_meta::LocalEventOptions::default(),
            )?;
        }
        let mode = plan.config().get("mode").and_then(Value::as_str);
        if mode == Some("block-activation") {
            let gate = self.cleanup_gate.as_ref().unwrap();
            gate.started.notify_one();
            gate.release.notified().await;
        }
        if matches!(mode, Some("bound" | "bound-fail")) {
            let counter = plan.local::<ProbeContract>()?;
            let before = counter.load(Ordering::SeqCst);
            plan.context().dispatch_local::<ProbeEvent>(())?;
            assert_eq!(
                counter.load(Ordering::SeqCst),
                before + 1,
                "consumer lost retained event listener"
            );
            let reply = plan
                .inject("test.portable")
                .unwrap()
                .clone()
                .invoke(rsi_meta::Message::new(b"bound".as_slice()))
                .await?;
            assert_eq!(reply.as_bytes(), b"bound");
        }
        if mode == Some("activate-fail")
            || mode == Some("bound-fail")
            || mode == Some("pending-activate-fail")
            || (mode == Some("activate-fail-once")
                && self.fail_once.fetch_add(1, Ordering::SeqCst) == 0)
            || (mode == Some("rollback-fail-after-first") && self.starts.load(Ordering::SeqCst) > 0)
        {
            return Err(MetaError::Activation("activation rejected".to_owned()));
        }
        if mode == Some("block-cleanup") {
            let gate = self.cleanup_gate.as_ref().ok_or_else(|| {
                MetaError::Activation("test cleanup gate is unavailable".to_owned())
            })?;
            let gate = Arc::clone(gate);
            plan.defer(
                "block test Profile cleanup",
                Box::new(move || {
                    Box::pin(async move {
                        gate.started.notify_one();
                        gate.release.notified().await;
                        Ok(())
                    })
                }),
            )?;
        }
        if mode == Some("cleanup-fail") {
            plan.defer(
                "reject fixture cleanup",
                Box::new(|| Box::pin(async { Err("cleanup rejected".into()) })),
            )?;
        }
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug)]
struct Resolver {
    starts: Arc<AtomicUsize>,
    fail_once: Arc<AtomicUsize>,
    prepare_calls: Arc<AtomicUsize>,
    mode: UpdateMode,
    cleanup_gate: Option<Arc<CleanupGate>>,
}

impl ProfileResolver for Resolver {
    fn validate_successor(&self, successor: &dyn ProfileResolver) -> rsi_meta_profile::Result<()> {
        assert_eq!(
            self.local_contract_type("test.local")?,
            successor.local_contract_type("test.local")?
        );
        assert_eq!(
            self.local_event_type("test.event")?,
            successor.local_event_type("test.event")?
        );
        Ok(())
    }
    fn resolve(&self, plugin: &rsi_meta::PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        if plugin.as_str() == "supply" {
            return Ok(ResolvedFactory::linked(
                plugin.clone(),
                "test",
                self.mode,
                Arc::new(SupplyFactory),
            ));
        }
        if plugin.as_str() != "probe" {
            return Err(rsi_meta_profile::ProfileError::UnknownPlugin {
                plugin: plugin.clone(),
            });
        }
        Ok(ResolvedFactory::linked(
            plugin.clone(),
            "test",
            self.mode,
            Arc::new(ProbeFactory {
                starts: Arc::clone(&self.starts),
                fail_once: Arc::clone(&self.fail_once),
                prepare_calls: Arc::clone(&self.prepare_calls),
                cleanup_gate: self.cleanup_gate.clone(),
            }),
        ))
    }

    fn local_contract_type(&self, key: &str) -> rsi_meta_profile::Result<std::any::TypeId> {
        if key == ProbeContract::KEY {
            Ok(std::any::TypeId::of::<ProbeContract>())
        } else {
            Err(rsi_meta_profile::ProfileError::UnknownLocalContract {
                key: key.to_owned(),
            })
        }
    }
    fn local_event_type(&self, key: &str) -> rsi_meta_profile::Result<std::any::TypeId> {
        if key == "test.event" {
            Ok(std::any::TypeId::of::<ProbeEvent>())
        } else {
            Err(rsi_meta_profile::ProfileError::UnknownLocalEvent {
                key: key.to_owned(),
            })
        }
    }
}

fn environment(root: &std::path::Path) -> ProfileEnvironment {
    ProfileEnvironment::new(
        root.join("config"),
        root.join("state"),
        root.join("cache"),
        "test",
        BTreeMap::new(),
    )
    .unwrap()
}

fn write_profile(path: &std::path::Path, mode: &str) {
    std::fs::write(
        path,
        format!(
            "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"one\"\nplugin = \"probe\"\nconfig = {{ mode = \"{mode}\" }}\n"
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn closing_control_joins_initial_activation_without_resurrecting_admission() {
    let temp = tempfile::tempdir().unwrap();
    write_profile(&temp.path().join("profile.toml"), "block-activation");
    let runtime = Runtime::new(RuntimeLimits::default()).unwrap();
    let gate = Arc::new(CleanupGate::default());
    let input = rsi_meta_profile::ProfileInput::new(
        Arc::new(Resolver {
            starts: Arc::new(AtomicUsize::new(0)),
            fail_once: Arc::new(AtomicUsize::new(0)),
            prepare_calls: Arc::new(AtomicUsize::new(0)),
            mode: UpdateMode::Replayable,
            cleanup_gate: Some(gate.clone()),
        }),
        ProfileProgram::from_file(temp.path().join("profile.toml")),
        environment(temp.path()),
        ProfileLimits::default(),
    );
    let bootstrap = ProfileBootstrap::prepare_input(&runtime, input.clone()).unwrap();
    let updater = bootstrap.updater();
    let control = bootstrap.control();
    let root = runtime.root();
    let activation = tokio::spawn(async move {
        root.apply(
            ResolvedFactory::linked(
                "rsi.meta.profile",
                "test",
                UpdateMode::RestartRequired,
                bootstrap.factory(),
            ),
            Value::Null,
        )
        .await
        .unwrap()
    });
    gate.started.notified().await;
    let closing = updater.close();
    tokio::pin!(closing);
    let closed_early = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(closing.as_mut(), cx).is_ready())
    })
    .await;
    gate.release.notify_one();
    let handle = activation.await.unwrap();
    if !closed_early {
        closing.await;
    }
    assert!(matches!(control.status().health(), ProfileHealth::Stopped));
    assert!(matches!(
        updater.submit(0, input),
        Err(rsi_meta_profile::ProfileError::Stopped)
    ));
    assert!(runtime.shutdown().await.is_clean());
    assert!(
        !closed_early,
        "close returned while initial activation was still running"
    );
    assert!(matches!(handle.snapshot().state, FiberState::Disposed));
}

#[tokio::test]
async fn ordinary_reload_keeps_successful_siblings_and_retries_only_failed_leaves() {
    fn write(path: &std::path::Path, middle: &str, revision: u8) {
        std::fs::write(
            path,
            format!(
                r#"format = 1
[[steps]]
kind = "plugin"
id = "first"
plugin = "probe"
config = {{ revision = {revision} }}
[[steps]]
kind = "plugin"
id = "middle"
plugin = "probe"
config = {{ mode = "{middle}" }}
[[steps]]
kind = "plugin"
id = "last"
plugin = "probe"
config = {{ revision = {revision} }}
"#
            ),
        )
        .unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write(&path, "ok", 1);
    let (runtime, handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    write(&path, "prepare-fail", 2);
    let outcome = control.reload().await.unwrap();
    assert!(
        matches!(outcome, ReloadOutcome::Partial { .. }),
        "{outcome:?}"
    );
    assert_eq!(outcome.status().health(), ProfileHealth::Degraded);
    assert_eq!(outcome.status().target().len(), 3);
    assert_eq!(outcome.status().observed().len(), 2);
    assert_eq!(starts.load(Ordering::SeqCst), 5);
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Partial { .. }
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 5);
    write(&path, "activate-fail", 2);
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Partial { .. }
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 5);
    write(&path, "ok", 2);
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 6);
    assert!(handle.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test(flavor = "current_thread")]
async fn live_and_rollback_preparation_leave_the_executor_available() {
    #[derive(Debug)]
    struct Gate {
        armed: std::sync::atomic::AtomicBool,
        entered: Notify,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    #[async_trait]
    impl PluginFactory for Gate {
        fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
            if self.armed.load(Ordering::SeqCst) {
                self.entered.notify_one();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| {
                        MetaError::InvalidConfig(
                            "async executor could not release preparation".into(),
                        )
                    })?;
            }
            Ok(PreparedActivation::new(desired.clone()))
        }
        async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
            if plan.config()["mode"] == "fail" {
                Err(MetaError::InvalidConfig("reject candidate".into()))
            } else {
                Ok(())
            }
        }
    }
    #[derive(Debug)]
    struct GatedResolver(Arc<Gate>);
    impl ProfileResolver for GatedResolver {
        fn validate_successor(&self, _: &dyn ProfileResolver) -> rsi_meta_profile::Result<()> {
            Ok(()) // This fixture has no nominal Local or event markers.
        }
        fn resolve(
            &self,
            plugin: &rsi_meta::PluginId,
        ) -> rsi_meta_profile::Result<ResolvedFactory> {
            Ok(ResolvedFactory::linked(
                plugin.clone(),
                "gate",
                UpdateMode::Replayable,
                self.0.clone(),
            ))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (release, receiver) = std::sync::mpsc::channel();
    let gate = Arc::new(Gate {
        armed: false.into(),
        entered: Notify::new(),
        release: Mutex::new(receiver),
    });
    let runtime = Runtime::default();
    let input = rsi_meta_profile::ProfileInput::new(
        Arc::new(GatedResolver(gate.clone())),
        ProfileProgram::from_file(&path).with_watch_policy(false.into()),
        environment(temp.path()),
        ProfileLimits::default(),
    );
    let bootstrap = ProfileBootstrap::prepare_input(&runtime, input.clone()).unwrap();
    let updater = bootstrap.updater();
    let _control = bootstrap.control();
    let handle = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.meta.profile",
                "test",
                UpdateMode::RestartRequired,
                bootstrap.factory(),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    write_profile(&path, "fail");
    let reload = tokio::spawn(async move { replace(&updater, &input).await });
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
            .await
            .expect("preparation did not enter");
        // This continuation runs on the sole async worker while prepare is blocked.
        release.send(()).unwrap();
    }
    assert!(matches!(
        reload.await.unwrap(),
        ReloadOutcome::RolledBack { .. }
    ));
    assert!(handle.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

fn write_isolated_pair(path: &std::path::Path, revision: usize, mode: &str) {
    std::fs::write(
        path,
        format!(
            r#"format = 1
[[steps]]
kind = "group"
id = "isolated"
[steps.isolation]
local = ["test.local"]
events = ["test.event"]
portable = ["test.portable"]
[[steps.nodes]]
kind = "plugin"
id = "provider"
plugin = "supply"
config = {{ all_lanes = true }}
[[steps.nodes]]
kind = "plugin"
id = "consumer"
plugin = "probe"
config = {{ mode = "{mode}", revision = {revision} }}
"#
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn isolated_suffix_replacement_and_rollback_keep_the_retained_provider_binding() {
    let clock = manual_clock::hold().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_isolated_pair(&path, 0, "bound");
    let (runtime, handle, control, starts, updater, input) = start_input_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        false,
    )
    .await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let original = control.status().observed()[0].clone();
    let provider = || {
        runtime.snapshot().fibers.into_iter().find(|fiber| {
        matches!(&fiber.factory, rsi_meta::FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == "supply")
    }).unwrap()
    };
    let original_fiber = provider();
    assert!(matches!(
        replace(&updater, &input).await,
        ReloadOutcome::Unchanged(_)
    ));
    write_isolated_pair(&path, 1, "bound");
    let outcome = replace(&updater, &input).await;
    assert!(matches!(outcome, ReloadOutcome::Applied(_)));
    assert!(
        matches!(
            outcome.status().observed()[1].state(),
            ProfileInstanceState::Active
        ),
        "{outcome:?}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(outcome.status().observed()[0], original);
    assert_eq!(provider(), original_fiber);
    write_isolated_pair(&path, 2, "bound-fail");
    let outcome = replace(&updater, &input).await;
    assert!(
        matches!(outcome, ReloadOutcome::RolledBack { .. }),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome.status().observed()[1].state(),
        ProfileInstanceState::Active
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    assert_eq!(outcome.status().observed()[0], original);
    assert_eq!(provider(), original_fiber);
    assert!(handle.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
    clock.finish().await;
}

fn write_ordered(path: &std::path::Path, ids: &[(&str, usize)]) {
    let mut source = "format = 1\n".to_owned();
    for (id, revision) in ids {
        write!(source,
                "[[steps]]\nkind = \"plugin\"\nid = \"{id}\"\nplugin = \"probe\"\nconfig = {{ revision = {revision}, order = \"{id}\" }}\n"
            ).unwrap();
    }
    std::fs::write(path, source).unwrap();
}

#[tokio::test]
async fn recovery_after_failed_compensation_retains_actual_positions_and_new_bindings() {
    #[derive(Debug)]
    struct AddedMarker(Resolver);
    enum NewMarker {}
    impl ProfileResolver for AddedMarker {
        fn resolve(
            &self,
            plugin: &rsi_meta::PluginId,
        ) -> rsi_meta_profile::Result<ResolvedFactory> {
            self.0.resolve(plugin)
        }
        fn local_contract_type(&self, key: &str) -> rsi_meta_profile::Result<std::any::TypeId> {
            if key == "new.marker" {
                Ok(std::any::TypeId::of::<NewMarker>())
            } else {
                self.0.local_contract_type(key)
            }
        }
        fn local_event_type(&self, key: &str) -> rsi_meta_profile::Result<std::any::TypeId> {
            self.0.local_event_type(key)
        }
    }
    let group = |source: String| {
        source.replace("[[steps]]", "[[steps.nodes]]").replacen("format = 1\n", "format = 1\n[[steps]]\nkind = \"group\"\nid = \"new-group\"\n[steps.isolation]\nlocal = [{key = \"new.marker\", label = \"new\"}]\n", 1)
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_ordered(&path, &[]);
    let (runtime, handle, control, starts, updater, _original) = start_input_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        false,
    )
    .await;
    let input = rsi_meta_profile::ProfileInput::new(
        Arc::new(AddedMarker(Resolver {
            starts: starts.clone(),
            fail_once: Arc::new(AtomicUsize::new(0)),
            prepare_calls: Arc::new(AtomicUsize::new(0)),
            mode: UpdateMode::Replayable,
            cleanup_gate: None,
        })),
        ProfileProgram::from_file(path.clone()).with_watch_policy(false.into()),
        environment(temp.path()),
        ProfileLimits::default(),
    );
    write_ordered(&path, &[("a", 0), ("b", 1), ("c", 2)]);
    let source = std::fs::read_to_string(&path)
        .unwrap()
        .replace("revision = 1", "revision = 1, mode = \"cleanup-fail\"")
        .replace("revision = 2", "revision = 2, mode = \"activate-fail\"");
    std::fs::write(&path, group(source)).unwrap();
    assert!(matches!(
        replace(&updater, &input).await,
        ReloadOutcome::Degraded { .. }
    ));
    assert_eq!(
        control
            .status()
            .observed()
            .iter()
            .map(|row| row.id().as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
    write_ordered(&path, &[("a", 0), ("d", 0)]);
    std::fs::write(&path, group(std::fs::read_to_string(&path).unwrap())).unwrap();
    assert!(matches!(
        replace(&updater, &input).await,
        ReloadOutcome::Applied(_)
    ));
    let order = Arc::new(Mutex::new(Vec::new()));
    runtime
        .root()
        .dispatch_local::<OrderEvent>(order.clone())
        .unwrap();
    assert_eq!(*order.lock().unwrap(), ["a", "d"]);
    assert_eq!(
        starts.load(Ordering::SeqCst),
        3,
        "the retained a fiber must not restart"
    );
    let report = handle.dispose().await;
    assert!(
        !report.is_clean(),
        "the injected cleanup failure remains evidence"
    );
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn exact_reload_reorders_and_replaces_only_the_changed_middle_leaf() {
    let clock = manual_clock::hold().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    let write = |ids: &[(&str, usize)]| write_ordered(&path, ids);
    write(&[("a", 0), ("b", 0), ("c", 0)]);
    let limited = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 4,
            maximum_composition_positions: 5,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let (runtime, handle, control, starts) = start_in_runtime(
        limited,
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
    )
    .await;
    let dispatch = || {
        let order = Arc::new(Mutex::new(Vec::new()));
        runtime
            .root()
            .dispatch_local::<OrderEvent>(order.clone())
            .unwrap();
        order.lock().unwrap().clone()
    };
    assert_eq!(dispatch(), ["a", "b", "c"]);
    let original = runtime.snapshot().fibers;
    write(&[("c", 0), ("b", 0), ("a", 0)]);
    let outcome = control.reload().await.unwrap();
    assert!(matches!(outcome, ReloadOutcome::Applied(_)), "{outcome:?}");
    assert_eq!(
        starts.load(Ordering::SeqCst),
        3,
        "reorder must keep all generations"
    );
    assert_eq!(runtime.snapshot().fibers, original);
    assert_eq!(dispatch(), ["c", "b", "a"]);
    assert_eq!(
        outcome
            .status()
            .observed()
            .iter()
            .map(|row| row.id().as_str())
            .collect::<Vec<_>>(),
        ["c", "b", "a"]
    );
    write(&[("c", 0), ("b", 1), ("a", 0)]);
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(
        starts.load(Ordering::SeqCst),
        4,
        "changing b must retain c and a"
    );
    assert_eq!(dispatch(), ["c", "b", "a"]);
    write(&[("c", 0), ("b", 2), ("a", 0)]);
    let failed = std::fs::read_to_string(&path)
        .unwrap()
        .replace("revision = 2", "revision = 2, mode = \"activate-fail\"");
    std::fs::write(&path, failed).unwrap();
    let outcome = control.reload().await.unwrap();
    assert!(
        matches!(outcome, ReloadOutcome::Partial { .. }),
        "{outcome:?}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 4);
    assert_eq!(dispatch(), ["c", "a"]);
    for revision in 0..32 {
        let middle = format!("new-{revision}");
        write(&[("c", 0), (&middle, 0), ("a", 0)]);
        assert!(matches!(
            control.reload().await.unwrap(),
            ReloadOutcome::Applied(_)
        ));
        assert_eq!(dispatch(), ["c", &middle, "a"]);
        assert_eq!(runtime.snapshot().fibers.len(), 4);
    }
    assert_eq!(starts.load(Ordering::SeqCst), 36);
    assert!(handle.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
    clock.finish().await;
}

#[tokio::test]
async fn restart_required_leaves_can_reorder_without_preparing_or_changing_generations() {
    let clock = manual_clock::hold().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_ordered(&path, &[("a", 0), ("b", 0)]);
    let (runtime, handle, control, starts) = start(temp.path(), UpdateMode::RestartRequired).await;
    let original = runtime.snapshot().fibers;
    write_ordered(&path, &[("b", 0), ("a", 0)]);
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(runtime.snapshot().fibers, original);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    write_ordered(&path, &[("b", 1), ("a", 0)]);
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::RestartRequired(_)
    ));
    assert_eq!(runtime.snapshot().fibers, original);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert!(handle.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    clock.finish().await;
}

fn named_pair_source(label: &str, mode: &str) -> String {
    let mut source = "format = 1\n".to_owned();
    for (group, id, plugin, config) in [
        (
            "providers",
            "provider",
            "supply",
            "{ all_lanes = true }".to_owned(),
        ),
        (
            "consumers",
            "consumer",
            "probe",
            format!("{{ mode = \"{mode}\" }}"),
        ),
    ] {
        write!(
            source,
            r#"
[[steps]]
kind = "group"
id = "{group}"
[steps.isolation]
local = [{{ key = "test.local", label = "{label}" }}]
events = [{{ key = "test.event", label = "{label}" }}]
portable = [{{ key = "test.portable", label = "{label}" }}]
[[steps.nodes]]
kind = "plugin"
id = "{id}"
plugin = "{plugin}"
config = {config}
"#
        )
        .unwrap();
    }
    source
}

#[tokio::test]
async fn named_groups_share_all_lanes_and_compensation_restores_exact_allocations() {
    let clock = manual_clock::hold().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    std::fs::write(&path, named_pair_source("shared", "bound")).unwrap();
    let (runtime, handle, _control, starts, updater, input) = start_input_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        false,
    )
    .await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    std::fs::write(&path, named_pair_source("other", "bound")).unwrap();
    assert!(matches!(
        replace(&updater, &input).await,
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    std::fs::write(&path, named_pair_source("failed", "bound-fail")).unwrap();
    let outcome = replace(&updater, &input).await;
    assert!(
        matches!(outcome, ReloadOutcome::RolledBack { .. }),
        "{outcome:?}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    assert!(
        outcome
            .status()
            .observed()
            .iter()
            .all(|leaf| matches!(leaf.state(), ProfileInstanceState::Active))
    );
    assert!(handle.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
    clock.finish().await;
}

#[tokio::test]
async fn overridden_ancestor_changes_and_group_moves_preserve_effective_fresh_bindings() {
    let clock = manual_clock::hold().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    let write = |parent: &str, label: &str| {
        write_isolated_pair(&path, 0, "bound");
        let inner = std::fs::read_to_string(&path)
            .unwrap()
            .replacen("format = 1\n", "", 1)
            .replace("[[steps]]", "[[steps.nodes]]")
            .replace("[steps.isolation]", "[steps.nodes.isolation]")
            .replace(
                "[[steps.nodes]]\nkind = \"plugin\"",
                "[[steps.nodes.nodes]]\nkind = \"plugin\"",
            );
        std::fs::write(&path, format!(
            "format = 1\n[[steps]]\nkind = \"group\"\nid = \"{parent}\"\n[steps.isolation]\nlocal = [{{key = \"test.local\", label = \"{label}\"}}]\n{inner}"
        )).unwrap();
    };
    write("outer", "one");
    let (runtime, handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    let original = runtime.snapshot().fibers;
    write("outer", "two");
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Unchanged(_)
    ));
    write("moved", "three");
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Unchanged(_)
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.snapshot().fibers, original);
    assert!(handle.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_complete());
    clock.finish().await;
}

async fn start(
    root: &std::path::Path,
    mode: UpdateMode,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn rsi_meta_profile::ProfileControl>,
    Arc<AtomicUsize>,
) {
    start_with_limits(root, mode, ProfileLimits::default()).await
}

async fn start_with_limits(
    root: &std::path::Path,
    mode: UpdateMode,
    limits: ProfileLimits,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn rsi_meta_profile::ProfileControl>,
    Arc<AtomicUsize>,
) {
    start_with_limits_and_cleanup_gate(root, mode, limits, None).await
}

async fn start_with_limits_and_cleanup_gate(
    root: &std::path::Path,
    mode: UpdateMode,
    limits: ProfileLimits,
    cleanup_gate: Option<Arc<CleanupGate>>,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn rsi_meta_profile::ProfileControl>,
    Arc<AtomicUsize>,
) {
    start_in_runtime(Runtime::default(), root, mode, limits, cleanup_gate).await
}

async fn start_in_runtime(
    runtime: Runtime,
    root: &std::path::Path,
    mode: UpdateMode,
    limits: ProfileLimits,
    cleanup_gate: Option<Arc<CleanupGate>>,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn rsi_meta_profile::ProfileControl>,
    Arc<AtomicUsize>,
) {
    start_program_in_runtime(runtime, root, mode, limits, cleanup_gate, true).await
}

async fn start_program_in_runtime(
    runtime: Runtime,
    root: &std::path::Path,
    mode: UpdateMode,
    limits: ProfileLimits,
    cleanup_gate: Option<Arc<CleanupGate>>,
    source_watch: impl Into<rsi_meta_profile::SourceWatchPolicy>,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn rsi_meta_profile::ProfileControl>,
    Arc<AtomicUsize>,
) {
    let (runtime, handle, control, starts, _, _) =
        start_input_in_runtime(runtime, root, mode, limits, cleanup_gate, source_watch).await;
    (runtime, handle, control, starts)
}

async fn replace(
    updater: &rsi_meta_profile::ProfileUpdateHandle,
    input: &rsi_meta_profile::ProfileInput,
) -> ReloadOutcome {
    updater
        .submit(updater.input_revision(), input.clone())
        .unwrap()
        .wait()
        .await
        .unwrap()
}

async fn start_input_in_runtime(
    runtime: Runtime,
    root: &std::path::Path,
    mode: UpdateMode,
    limits: ProfileLimits,
    cleanup_gate: Option<Arc<CleanupGate>>,
    source_watch: impl Into<rsi_meta_profile::SourceWatchPolicy>,
) -> (
    Runtime,
    rsi_meta::FiberHandle,
    Arc<dyn rsi_meta_profile::ProfileControl>,
    Arc<AtomicUsize>,
    rsi_meta_profile::ProfileUpdateHandle,
    rsi_meta_profile::ProfileInput,
) {
    let starts = Arc::new(AtomicUsize::new(0));
    let resolver = Arc::new(Resolver {
        starts: Arc::clone(&starts),
        fail_once: Arc::new(AtomicUsize::new(0)),
        prepare_calls: Arc::new(AtomicUsize::new(0)),
        mode,
        cleanup_gate,
    });
    let input = rsi_meta_profile::ProfileInput::new(
        resolver,
        ProfileProgram::from_file(root.join("profile.toml")).with_watch_policy(source_watch.into()),
        environment(root),
        limits,
    );
    let bootstrap = ProfileBootstrap::prepare_input(&runtime, input.clone()).unwrap();
    let updater = bootstrap.updater();
    let control = bootstrap.control();
    let handle = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.meta.profile",
                "test",
                UpdateMode::RestartRequired,
                bootstrap.factory(),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(handle.snapshot().state, FiberState::Active));
    (runtime, handle, control, starts, updater, input)
}

#[tokio::test]
async fn late_child_failure_is_observed_and_its_diagnostic_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    write_profile(&temp.path().join("profile.toml"), "pending-activate-fail");
    let limits = ProfileLimits {
        maximum_diagnostic_bytes: 8,
        ..ProfileLimits::default()
    };
    let (runtime, _handle, control, _) =
        start_with_limits(temp.path(), UpdateMode::Replayable, limits).await;
    let mut changes = control.subscribe();
    assert!(matches!(
        changes.borrow().observed()[0].state(),
        ProfileInstanceState::Pending(_)
    ));

    let _supply_handle = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "test.probe-supply",
                "test",
                UpdateMode::Replayable,
                Arc::new(SupplyFactory),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            changes.changed().await.unwrap();
            let status = changes.borrow_and_update().clone();
            if status.health() == ProfileHealth::Degraded {
                break status;
            }
        }
    })
    .await
    .expect("the Profile observer must not miss a child transition");

    assert!(status.diagnostic().unwrap().len() <= 8);
    assert_eq!(status.observed()[0].state(), &ProfileInstanceState::Failed);
    assert_eq!(control.status().health(), ProfileHealth::Degraded);
    assert_eq!(control.status().diagnostic(), status.diagnostic());
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn preflight_failure_advances_observation_without_advancing_graph() {
    let clock = manual_clock::hold().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, _) = start(temp.path(), UpdateMode::Replayable).await;
    let before = control.status().revision();
    let graph = control.snapshot().revision();
    std::fs::write(&path, "not valid TOML = [").unwrap();
    assert!(control.reload().await.is_err());
    let failed = control.status();
    assert!(failed.revision() > before);
    assert!(failed.diagnostic().is_some());
    assert_eq!(control.snapshot().revision(), graph);
    assert_eq!(control.status().revision(), failed.revision());
    let _ = runtime.shutdown().await;
    clock.finish().await;
}

#[tokio::test]
async fn control_is_a_typed_local_service_and_healthy_equal_tree_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    write_profile(&temp.path().join("profile.toml"), "ok");
    let (runtime, handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(control.status().revision(), 1);
    assert_eq!(control.status().health(), ProfileHealth::Converged);
    assert!(
        runtime
            .root()
            .lookup_local::<ProfileControlContract>()
            .is_some()
    );

    let outcome = control.reload().await.unwrap();
    assert!(matches!(outcome, ReloadOutcome::Unchanged(_)));
    assert_eq!(control.snapshot().revision(), 1);
    assert_eq!(
        outcome.status().last_attempt().unwrap().outcome,
        rsi_meta_profile::ProfileAttemptOutcome::Unchanged
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(control.snapshot().nodes().len(), 1);

    let _ = handle.dispose().await;
    assert_eq!(control.status().health(), ProfileHealth::Stopped);
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn just_in_time_preparation_and_failed_apply_replay_the_old_target() {
    let clock = manual_clock::hold().await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, starts, updater, input) = start_input_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        false,
    )
    .await;

    write_profile(&path, "prepare-fail");
    let outcome = replace(&updater, &input).await;
    assert!(matches!(outcome, ReloadOutcome::RolledBack { .. }));
    assert!(matches!(&outcome, ReloadOutcome::RolledBack { error, .. }
        if error.contains("candidate rejected")));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(control.snapshot().revision(), 2);
    assert_eq!(
        outcome.status().last_attempt().unwrap().failure,
        Some(rsi_meta_profile::ProfileFailureKind::Prepare)
    );

    write_profile(&path, "activate-fail");
    let outcome = replace(&updater, &input).await;
    assert!(matches!(outcome, ReloadOutcome::RolledBack { .. }));
    assert!(matches!(&outcome, ReloadOutcome::RolledBack { error, .. }
        if error.contains("activation rejected")));
    assert_eq!(control.snapshot().revision(), 3);
    assert_eq!(
        outcome.status().last_attempt().unwrap().failure,
        Some(rsi_meta_profile::ProfileFailureKind::Apply)
    );
    assert_eq!(outcome.status().health(), ProfileHealth::Converged);
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    let _ = runtime.shutdown().await;
    clock.finish().await;
}

#[tokio::test]
async fn rollback_preparation_failure_degrades_and_is_published_in_profile_status() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "old-prepare-fails-on-reload");
    let (runtime, _handle, control, starts, updater, input) = start_input_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        false,
    )
    .await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    write_profile(&path, "activate-fail");
    let outcome = replace(&updater, &input).await;
    assert!(matches!(outcome, ReloadOutcome::Degraded { .. }));
    let status = control.status();
    assert_eq!(status.health(), ProfileHealth::Degraded);
    assert!(
        status.diagnostic().is_some(),
        "a reload failure returned to the caller must also be observable"
    );
    assert!(status.diagnostic().unwrap().contains("activation rejected"));
    assert!(
        matches!(outcome, ReloadOutcome::Degraded { rollback_error, .. }
        if rollback_error.contains("rollback preparation rejected"))
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn failed_restart_required_leaves_retry_unchanged_source() {
    for mode in ["prepare-fail", "activate-fail-once"] {
        let temp = tempfile::tempdir().unwrap();
        write_profile(&temp.path().join("profile.toml"), mode);
        let (runtime, _handle, control, starts) =
            start(temp.path(), UpdateMode::RestartRequired).await;
        assert_eq!(control.status().health(), ProfileHealth::Degraded);
        let outcome = control.reload().await.unwrap();
        if mode == "prepare-fail" {
            assert!(
                matches!(outcome, ReloadOutcome::Partial { .. }),
                "{outcome:?}"
            );
            assert!(outcome.status().diagnostic().is_some());
        } else {
            assert!(matches!(outcome, ReloadOutcome::Applied(_)), "{outcome:?}");
            assert_eq!(starts.load(Ordering::SeqCst), 1);
        }
        let _ = runtime.shutdown().await;
    }
}

#[tokio::test]
async fn replacement_after_partial_reload_restores_only_prior_usable_membership() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, starts, updater, input) = start_input_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        false,
    )
    .await;
    write_profile(&path, "prepare-fail");
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Partial { .. }
    ));
    let before = control.status();
    assert!(control.status().observed().is_empty());
    write_profile(&path, "activate-fail");
    let outcome = replace(&updater, &input).await;
    assert!(
        matches!(outcome, ReloadOutcome::RolledBack { .. }),
        "{outcome:?}"
    );
    assert!(control.status().observed().is_empty());
    assert_eq!(outcome.status().health(), ProfileHealth::Degraded);
    assert_eq!(outcome.status().diagnostic(), before.diagnostic());
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    write_profile(&path, "ok");
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn reloads_at_the_exact_profile_and_leaf_fiber_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let runtime = Runtime::new(RuntimeLimits {
        topology: TopologyLimits {
            maximum_fibers: 2,
            ..TopologyLimits::default()
        },
        ..RuntimeLimits::default()
    })
    .unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let resolver = Arc::new(Resolver {
        starts: Arc::clone(&starts),
        fail_once: Arc::new(AtomicUsize::new(0)),
        prepare_calls: Arc::new(AtomicUsize::new(0)),
        mode: UpdateMode::Replayable,
        cleanup_gate: None,
    });
    let bootstrap = ProfileBootstrap::prepare(
        &runtime,
        resolver,
        ProfileProgram::from_file(&path),
        environment(temp.path()),
        ProfileLimits::default(),
    )
    .unwrap();
    let control = bootstrap.control();
    let _handle = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.meta.profile",
                "test",
                UpdateMode::RestartRequired,
                bootstrap.factory(),
            ),
            Value::Null,
        )
        .await
        .unwrap();

    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Unchanged(_)
    ));
    write_profile(&path, "changed");
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn restart_required_publishes_digest_without_mutating_and_pending_is_usable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::RestartRequired).await;
    let old_digest = control.status().source_digest().to_owned();
    let old_revision = control.status().revision();
    write_profile(&path, "pending");
    let outcome = control.reload().await.unwrap();
    assert!(matches!(outcome, ReloadOutcome::RestartRequired(_)));
    assert_ne!(outcome.status().source_digest(), old_digest);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let repeated = control.reload().await.unwrap();
    assert!(matches!(repeated, ReloadOutcome::RestartRequired(_)));
    assert!(repeated.status().revision() > old_revision);
    assert_eq!(control.snapshot().revision(), 1);
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "reloading the same restart-only candidate must not apply it live"
    );
    write_profile(&path, "ok");
    let reverted = control.reload().await.unwrap();
    assert!(matches!(reverted, ReloadOutcome::Unchanged(_)));
    assert_eq!(reverted.status().health(), ProfileHealth::Converged);
    assert!(reverted.status().revision() > repeated.status().revision());
    assert_eq!(control.snapshot().revision(), 1);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let _ = runtime.shutdown().await;

    write_profile(&path, "pending");
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(matches!(
        control.status().observed()[0].state(),
        ProfileInstanceState::Pending(_)
    ));
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn source_watch_leases_enable_existing_profile_and_last_drop_stops_observation() {
    async fn observed(starts: &AtomicUsize, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while starts.load(Ordering::SeqCst) != count {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("leased source change must converge");
    }
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let policy = rsi_meta_profile::SourceWatchPolicy::leased();
    let (runtime, _handle, control, starts) = start_program_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        policy.clone(),
    )
    .await;
    write_profile(&path, "one");
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let first = policy.acquire().unwrap();
    let second = policy.acquire().unwrap();
    observed(&starts, 2).await;
    drop(first);
    write_profile(&path, "two");
    observed(&starts, 3).await;
    drop(second);
    assert!(!policy.enabled());
    write_profile(&path, "three");
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    let resumed = policy.acquire().unwrap();
    observed(&starts, 4).await;
    drop(resumed);
    write_profile(&path, "four");
    control.reload().await.unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 5);
    assert!(runtime.shutdown().await.is_complete());
}

#[tokio::test]
async fn manual_source_policy_keeps_active_graph_until_explicit_reload() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, starts) = start_program_in_runtime(
        Runtime::default(),
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        None,
        false,
    )
    .await;
    assert_eq!(control.status().watcher(), WatcherHealth::Inactive);
    write_profile(&path, "new");
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(control.status().last_attempt().is_none());
    control.reload().await.unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(control.status().watcher(), WatcherHealth::Inactive);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn watcher_reloads_changed_sources_and_subscription_observes_completion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    let mut changes = control.subscribe();
    write_profile(&path, "new");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            changes.changed().await.unwrap();
            if changes.borrow().last_attempt().is_some() {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn convergence_publishes_only_complete_observed_graphs() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "block-cleanup");
    let gate = Arc::new(CleanupGate::default());
    let (runtime, _handle, control, _) = start_with_limits_and_cleanup_gate(
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        Some(Arc::clone(&gate)),
    )
    .await;
    let changes = control.subscribe();

    write_profile(&path, "changed");
    let reload = tokio::spawn({
        let control = Arc::clone(&control);
        async move { control.reload().await }
    });
    gate.started.notified().await;

    let direct = control.status();
    let published = changes.borrow().clone();
    gate.release.notify_one();
    let outcome = reload.await.unwrap().unwrap();

    assert_eq!(direct.health(), ProfileHealth::Converging);
    assert_eq!(direct.observed().len(), 1);
    assert_eq!(published.health(), ProfileHealth::Converging);
    assert_eq!(published.observed().len(), 1);
    assert!(matches!(outcome, ReloadOutcome::Applied(_)));
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn watcher_fault_during_convergence_never_publishes_a_partial_graph() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "block-cleanup");
    let gate = Arc::new(CleanupGate::default());
    let (runtime, _handle, control, _) = start_with_limits_and_cleanup_gate(
        temp.path(),
        UpdateMode::Replayable,
        ProfileLimits::default(),
        Some(Arc::clone(&gate)),
    )
    .await;
    let mut changes = control.subscribe();

    write_profile(&path, "changed");
    let reload = tokio::spawn({
        let control = Arc::clone(&control);
        async move { control.reload().await }
    });
    gate.started.notified().await;
    let converging = changes.borrow_and_update().clone();
    assert_eq!(converging.health(), ProfileHealth::Converging);
    assert_eq!(converging.observed().len(), 1);

    std::fs::remove_file(&path).unwrap();
    let publication = tokio::time::timeout(Duration::from_millis(500), changes.changed())
        .await
        .ok()
        .map(|result| {
            result.unwrap();
            changes.borrow_and_update().clone()
        });
    let status = publication.unwrap_or_else(|| changes.borrow().clone());
    assert_eq!(status.health(), ProfileHealth::Converging);
    assert_eq!(
        status.observed(),
        converging.observed(),
        "a queued watcher fault cannot expose a partial observed graph"
    );
    write_profile(&path, "changed");
    gate.release.notify_one();
    let outcome = reload.await.unwrap().unwrap();
    assert!(matches!(outcome, ReloadOutcome::Applied(_)));
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn watcher_backs_off_before_retrying_the_same_invalid_source() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, _) = start(temp.path(), UpdateMode::Replayable).await;
    let mut changes = control.subscribe();
    std::fs::write(&path, "format =\n").unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            changes.changed().await.unwrap();
            let status = changes.borrow_and_update();
            if status.diagnostic().is_some() {
                break;
            }
        }
    })
    .await
    .expect("the invalid source is observed once");
    assert!(
        tokio::time::timeout(Duration::from_millis(400), changes.changed())
            .await
            .is_err(),
        "an unchanged invalid candidate must not be recompiled at the 100 ms watch interval"
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn watcher_does_not_reapply_the_same_candidate_after_partial_success() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    write_profile(&path, "activate-fail");

    tokio::time::timeout(Duration::from_secs(5), async {
        while control.status().last_attempt().is_none_or(|attempt| {
            attempt.outcome != rsi_meta_profile::ProfileAttemptOutcome::Partial
        }) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the first failed candidate reports partial success");
    let attempt = control.status().last_attempt().unwrap().sequence;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        control.status().last_attempt().unwrap().sequence,
        attempt,
        "the same source candidate must wait for another source change or manual reload"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn watcher_fault_is_visible_and_restored_required_source_retries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    std::fs::remove_file(&path).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if control.status().watcher() == rsi_meta_profile::WatcherHealth::Faulted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let error = control.reload().await.unwrap_err();
    assert!(!error.to_string().contains(path.to_string_lossy().as_ref()));
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    write_profile(&path, "restored");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = control.status();
            if status.revision() >= 2
                && status.watcher() == rsi_meta_profile::WatcherHealth::Healthy
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn degraded_same_source_is_not_suppressed_and_can_converge_on_retry() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "rollback-fail-after-first");
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    write_profile(&path, "activate-fail-once");
    let first = control.reload().await.unwrap();
    assert!(matches!(first, ReloadOutcome::Partial { .. }));
    assert_eq!(first.status().health(), ProfileHealth::Degraded);
    assert!(
        first
            .status()
            .diagnostic()
            .unwrap()
            .contains("activation rejected")
    );

    let second = control.reload().await.unwrap();
    // A watcher already queued behind the failed manual reload may perform the
    // retry first. An unchanged response is valid only with the recovered graph.
    assert!(
        matches!(
            second,
            ReloadOutcome::Applied(_) | ReloadOutcome::Unchanged(_)
        ),
        "{second:?}"
    );
    assert_eq!(second.status().health(), ProfileHealth::Converged);
    assert!(
        matches!(second.status().observed(), [instance] if *instance.state() == ProfileInstanceState::Active)
    );
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let _ = runtime.shutdown().await;
}

#[test]
fn control_object_is_send_sync() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn rsi_meta_profile::ProfileControl>();
    let _ = Mutex::new(());
}

#[derive(Debug)]
struct PanickingSuccessor;
impl ProfileResolver for PanickingSuccessor {
    fn resolve(&self, _: &rsi_meta::PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        unreachable!("empty profile")
    }
    fn validate_successor(&self, _: &dyn ProfileResolver) -> rsi_meta_profile::Result<()> {
        panic!("fixture successor panic");
    }
}
#[tokio::test]
async fn panicked_update_settles_tickets_closes_admission_and_reports_cleanup_failure() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = Runtime::default();
    let input = rsi_meta_profile::ProfileInput::new(
        Arc::new(PanickingSuccessor),
        ProfileProgram::from_profile(rsi_meta_profile::Profile::default()),
        environment(temp.path()),
        ProfileLimits::default(),
    );
    let bootstrap = ProfileBootstrap::prepare_input(&runtime, input.clone()).unwrap();
    let updater = bootstrap.updater();
    let handle = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "profile",
                "1",
                UpdateMode::RestartRequired,
                bootstrap.factory(),
            ),
            Value::Null,
        )
        .await
        .unwrap();
    let ticket = updater
        .submit(updater.input_revision(), input.clone())
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), ticket.wait())
        .await
        .unwrap();
    assert!(matches!(
        result,
        Err(rsi_meta_profile::ProfileError::Stopped)
    ));
    assert!(matches!(
        updater.submit(updater.input_revision(), input),
        Err(rsi_meta_profile::ProfileError::Stopped)
    ));
    assert!(
        !handle.dispose().await.is_clean(),
        "worker panic must be reported by cleanup"
    );
    runtime.shutdown().await;
}

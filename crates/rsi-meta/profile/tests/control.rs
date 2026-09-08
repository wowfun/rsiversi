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
            return Err(MetaError::InvalidConfig("secret candidate".to_owned()));
        }
        if mode == "old-prepare-fails-on-reload" && prepare_call > 0 {
            return Err(MetaError::InvalidConfig("secret rollback".to_owned()));
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
            return Err(MetaError::Activation("secret activation".to_owned()));
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
    let (runtime, handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let original = control.status().observed()[0].clone();
    let provider = || {
        runtime.snapshot().fibers.into_iter().find(|fiber| {
        matches!(&fiber.factory, rsi_meta::FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == "supply")
    }).unwrap()
    };
    let original_fiber = provider();
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Unchanged(_)
    ));
    write_isolated_pair(&path, 1, "bound");
    let outcome = control.reload().await.unwrap();
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
    let outcome = control.reload().await.unwrap();
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
        matches!(outcome, ReloadOutcome::RolledBack { .. }),
        "{outcome:?}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 5);
    assert_eq!(dispatch(), ["c", "b", "a"]);
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
    assert_eq!(starts.load(Ordering::SeqCst), 37);
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
    let (runtime, handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    std::fs::write(&path, named_pair_source("other", "bound")).unwrap();
    assert!(matches!(
        control.reload().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    std::fs::write(&path, named_pair_source("failed", "bound-fail")).unwrap();
    let outcome = control.reload().await.unwrap();
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
    let starts = Arc::new(AtomicUsize::new(0));
    let resolver = Arc::new(Resolver {
        starts: Arc::clone(&starts),
        fail_once: Arc::new(AtomicUsize::new(0)),
        prepare_calls: Arc::new(AtomicUsize::new(0)),
        mode,
        cleanup_gate,
    });
    let bootstrap = ProfileBootstrap::prepare(
        &runtime,
        resolver,
        ProfileProgram::from_file(root.join("profile.toml")),
        environment(root),
        limits,
    )
    .unwrap();
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
    (runtime, handle, control, starts)
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
    let _ = runtime.shutdown().await;
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
    assert_eq!(outcome.status().revision(), 1);
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
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;

    write_profile(&path, "prepare-fail");
    let outcome = control.reload().await.unwrap();
    assert!(matches!(outcome, ReloadOutcome::RolledBack { .. }));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(control.status().revision(), 2);

    write_profile(&path, "activate-fail");
    let outcome = control.reload().await.unwrap();
    assert!(matches!(outcome, ReloadOutcome::RolledBack { .. }));
    assert_eq!(outcome.status().revision(), 3);
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
    let (runtime, _handle, control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    write_profile(&path, "activate-fail");
    let outcome = control.reload().await.unwrap();
    assert!(matches!(outcome, ReloadOutcome::Degraded { .. }));
    let status = control.status();
    assert_eq!(status.health(), ProfileHealth::Degraded);
    assert!(
        status.diagnostic().is_some(),
        "a reload failure returned to the caller must also be observable"
    );
    assert!(!status.diagnostic().unwrap().contains("secret"));
    assert!(!format!("{status:?}").contains("secret activation"));
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
    write_profile(&path, "pending");
    let outcome = control.reload().await.unwrap();
    assert!(matches!(outcome, ReloadOutcome::RestartRequired(_)));
    assert_ne!(outcome.status().source_digest(), old_digest);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let repeated = control.reload().await.unwrap();
    assert!(matches!(repeated, ReloadOutcome::RestartRequired(_)));
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "reloading the same restart-only candidate must not apply it live"
    );
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
            if changes.borrow().revision() >= 2 {
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
    write_profile(&path, "changed");
    gate.release.notify_one();
    let outcome = reload.await.unwrap().unwrap();

    if let Some(status) = publication {
        assert_eq!(status.watcher(), WatcherHealth::Faulted);
        assert_eq!(
            status.observed().len(),
            1,
            "watcher diagnostics must retain the last complete observed graph"
        );
    }
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
async fn watcher_does_not_reapply_the_same_candidate_after_successful_rollback() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("profile.toml");
    write_profile(&path, "ok");
    let (runtime, _handle, _control, starts) = start(temp.path(), UpdateMode::Replayable).await;
    write_profile(&path, "activate-fail");

    tokio::time::timeout(Duration::from_secs(5), async {
        while starts.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the first failed candidate is rolled back");
    let after_rollback = starts.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        starts.load(Ordering::SeqCst),
        after_rollback,
        "the same source candidate must wait for another source change or manual reload"
    );
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
    assert!(matches!(first, ReloadOutcome::Degraded { .. }));
    assert_eq!(first.status().health(), ProfileHealth::Degraded);
    assert!(!first.status().diagnostic().unwrap().contains("secret"));

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

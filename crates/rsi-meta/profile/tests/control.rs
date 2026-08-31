use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberState, LocalContract, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, Runtime, RuntimeLimits, TopologyLimits, UpdateMode,
};
use rsi_meta_profile::{
    IsolationSpec, ProfileBootstrap, ProfileControlContract, ProfileEnvironment, ProfileHealth,
    ProfileInstanceState, ProfileLimits, ProfileProgram, ProfileResolver, ReloadOutcome,
    WatcherHealth,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

enum ProbeContract {}

impl LocalContract for ProbeContract {
    const KEY: &'static str = "test.local";
    type Service = AtomicUsize;
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
        let supply = plan
            .context()
            .provide_local::<ProbeContract>(Arc::new(AtomicUsize::new(0)))?;
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
        if mode == "pending" || mode == "pending-activate-fail" {
            return Ok(prepared.requiring_local::<ProbeContract>());
        }
        Ok(prepared)
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let mode = plan.config().get("mode").and_then(Value::as_str);
        if mode == Some("activate-fail")
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

    fn isolate(
        &self,
        mut context: Context,
        isolation: &IsolationSpec,
    ) -> rsi_meta_profile::Result<Context> {
        for key in isolation.local() {
            if key != ProbeContract::KEY {
                return Err(rsi_meta_profile::ProfileError::UnknownLocalContract {
                    key: key.clone(),
                });
            }
            context = context.isolate_local_fresh::<ProbeContract>()?.0;
        }
        if !isolation.events().is_empty() || !isolation.portable().is_empty() {
            return Err(rsi_meta_profile::ProfileError::InvalidProgram(
                "test resolver accepts only Local isolation".to_owned(),
            ));
        }
        Ok(context)
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
    let runtime = Runtime::default();
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
    assert!(matches!(second, ReloadOutcome::Applied(_)));
    assert_eq!(second.status().health(), ProfileHealth::Converged);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let _ = runtime.shutdown().await;
}

#[test]
fn control_object_is_send_sync() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn rsi_meta_profile::ProfileControl>();
    let _ = Mutex::new(());
}

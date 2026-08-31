use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FiberState, LocalContract, PluginFactory, PluginId,
    PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_meta_profile::{
    IsolationSpec, Profile, ProfileCompiler, ProfileEntry, ProfileEnvironment,
    ProfileGenerationPlan, ProfileLimits, ProfileProgram, ProfileResolver,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct MissingResolver;

impl ProfileResolver for MissingResolver {
    fn resolve(&self, plugin: &PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        Err(rsi_meta_profile::ProfileError::UnknownPlugin {
            plugin: plugin.clone(),
        })
    }

    fn isolate(
        &self,
        _context: Context,
        _isolation: &IsolationSpec,
    ) -> rsi_meta_profile::Result<Context> {
        unreachable!("unknown factories fail before Context isolation")
    }
}

#[derive(Debug)]
struct OneFactoryResolver {
    live: Arc<AtomicUsize>,
}

impl ProfileResolver for OneFactoryResolver {
    fn resolve(&self, plugin: &PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        if plugin.as_str() != "test.effect" {
            return Err(rsi_meta_profile::ProfileError::UnknownPlugin {
                plugin: plugin.clone(),
            });
        }
        Ok(ResolvedFactory::linked(
            plugin.clone(),
            "1",
            UpdateMode::Replayable,
            Arc::new(EffectFactory {
                live: Arc::clone(&self.live),
            }),
        ))
    }

    fn isolate(
        &self,
        context: Context,
        isolation: &IsolationSpec,
    ) -> rsi_meta_profile::Result<Context> {
        assert!(isolation.local().is_empty());
        assert!(isolation.events().is_empty());
        assert!(isolation.portable().is_empty());
        Ok(context)
    }
}

#[derive(Debug)]
struct EffectFactory {
    live: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for EffectFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.live.fetch_add(1, Ordering::SeqCst);
        let live = Arc::clone(&self.live);
        plan.defer(
            "release generation test effect",
            Box::new(move || {
                Box::pin(async move {
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct SingleResolver {
    plugin: &'static str,
    factory: Arc<dyn PluginFactory>,
}

impl ProfileResolver for SingleResolver {
    fn resolve(&self, plugin: &PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        if plugin.as_str() != self.plugin {
            return Err(rsi_meta_profile::ProfileError::UnknownPlugin {
                plugin: plugin.clone(),
            });
        }
        Ok(ResolvedFactory::linked(
            plugin.clone(),
            "1",
            UpdateMode::Replayable,
            Arc::clone(&self.factory),
        ))
    }

    fn isolate(
        &self,
        context: Context,
        isolation: &IsolationSpec,
    ) -> rsi_meta_profile::Result<Context> {
        assert!(isolation.local().is_empty());
        assert!(isolation.events().is_empty());
        assert!(isolation.portable().is_empty());
        Ok(context)
    }
}

#[derive(Debug)]
struct PrepareFailureFactory {
    calls: Arc<AtomicUsize>,
}

enum MissingContract {}

impl LocalContract for MissingContract {
    const KEY: &'static str = "test.missing";
    type Service = ();
}

#[derive(Debug)]
struct PendingFactory;

#[async_trait]
impl PluginFactory for PendingFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()).requiring_local::<MissingContract>())
    }

    async fn activate(&self, _plan: ActivationPlan) -> rsi_meta::Result<()> {
        unreachable!("a leaf with an unsatisfied requirement cannot activate")
    }
}

#[derive(Debug)]
struct BlockingFactory {
    entered: Arc<Notify>,
    live: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for BlockingFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.live.fetch_add(1, Ordering::SeqCst);
        let live = Arc::clone(&self.live);
        plan.defer(
            "release blocking generation test effect",
            Box::new(move || {
                Box::pin(async move {
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[derive(Debug)]
struct BlockingPreparationFactory {
    entered: Arc<Notify>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

#[async_trait]
impl PluginFactory for BlockingPreparationFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.entered.notify_one();
        let (released, ready) = &*self.release;
        let mut released = released.lock().expect("preparation release poisoned");
        while !*released {
            released = ready.wait(released).expect("preparation release poisoned");
        }
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, _plan: ActivationPlan) -> rsi_meta::Result<()> {
        unreachable!("cancelled preparation cannot activate")
    }
}

#[derive(Debug)]
struct ActivationFailureResolver {
    live: Arc<AtomicUsize>,
}

impl ProfileResolver for ActivationFailureResolver {
    fn resolve(&self, plugin: &PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        let factory: Arc<dyn PluginFactory> = match plugin.as_str() {
            "test.effect" => Arc::new(EffectFactory {
                live: Arc::clone(&self.live),
            }),
            "test.activate-failure" => Arc::new(ActivationFailureFactory {
                live: Arc::clone(&self.live),
            }),
            _ => {
                return Err(rsi_meta_profile::ProfileError::UnknownPlugin {
                    plugin: plugin.clone(),
                });
            }
        };
        Ok(ResolvedFactory::linked(
            plugin.clone(),
            "1",
            UpdateMode::Replayable,
            factory,
        ))
    }

    fn isolate(
        &self,
        context: Context,
        isolation: &IsolationSpec,
    ) -> rsi_meta_profile::Result<Context> {
        assert!(isolation.local().is_empty());
        assert!(isolation.events().is_empty());
        assert!(isolation.portable().is_empty());
        Ok(context)
    }
}

#[derive(Debug)]
struct ActivationFailureFactory {
    live: Arc<AtomicUsize>,
}

#[async_trait]
impl PluginFactory for ActivationFailureFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.live.fetch_add(1, Ordering::SeqCst);
        let live = Arc::clone(&self.live);
        plan.defer(
            "release failing generation test effect",
            Box::new(move || {
                Box::pin(async move {
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        Err(rsi_meta::MetaError::Activation(
            "secret activation detail".to_owned(),
        ))
    }
}

#[async_trait]
impl PluginFactory for PrepareFailureFactory {
    fn prepare(&self, _desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(rsi_meta::MetaError::InvalidConfig(
            "secret preparation detail".to_owned(),
        ))
    }

    async fn activate(&self, _plan: ActivationPlan) -> rsi_meta::Result<()> {
        unreachable!("a rejected preparation cannot activate")
    }
}

fn compiler(root: &std::path::Path) -> ProfileCompiler {
    let environment = ProfileEnvironment::new(
        root.join("config"),
        root.join("state"),
        root.join("cache"),
        "test",
        BTreeMap::new(),
    )
    .unwrap();
    ProfileCompiler::new(environment, ProfileLimits::default())
}

#[test]
fn unknown_factory_is_rejected_while_resolving_the_opaque_plan() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("missing", "missing.plugin", Value::Null),
        ])))
        .unwrap();

    let error = ProfileGenerationPlan::resolve(candidate, Arc::new(MissingResolver)).unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_profile::ProfileError::UnknownPlugin { plugin }
            if plugin.as_str() == "missing.plugin"
    ));
}

#[tokio::test]
async fn activation_is_owned_by_one_disposable_wrapper_fiber() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("effect", "test.effect", Value::Null),
        ])))
        .unwrap();
    let live = Arc::new(AtomicUsize::new(0));
    let plan = ProfileGenerationPlan::resolve(
        candidate,
        Arc::new(OneFactoryResolver {
            live: Arc::clone(&live),
        }),
    )
    .unwrap();
    let runtime = Runtime::default();
    let baseline = runtime.resource_snapshot().fibers.current;

    let generation = plan
        .activate(&runtime.root(), &CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(generation.snapshot().state, FiberState::Active));
    assert_eq!(live.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.resource_snapshot().fibers.current, baseline + 2);
    assert!(generation.dispose().await.is_clean());
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.resource_snapshot().fibers.current, baseline);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn returned_wrapper_cannot_reconfigure_into_an_empty_generation() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("effect", "test.effect", Value::Null),
        ])))
        .unwrap();
    let live = Arc::new(AtomicUsize::new(0));
    let plan = ProfileGenerationPlan::resolve(
        candidate,
        Arc::new(OneFactoryResolver {
            live: Arc::clone(&live),
        }),
    )
    .unwrap();
    let runtime = Runtime::default();
    let baseline = runtime.resource_snapshot();
    let generation = plan
        .activate(&runtime.root(), &CancellationToken::new())
        .await
        .unwrap();

    let reconfigured = generation.reconfigure(Value::Null).await.unwrap();

    assert!(matches!(reconfigured.state, FiberState::Failed(_)));
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(generation.dispose().await.is_clean());
    assert_eq!(
        runtime.resource_snapshot().fibers.current,
        baseline.fibers.current
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn preparation_failure_is_redacted_before_wrapper_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("rejected", "test.prepare-failure", Value::Null),
        ])))
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn PluginFactory> = Arc::new(PrepareFailureFactory {
        calls: Arc::clone(&calls),
    });
    let plan = ProfileGenerationPlan::resolve(
        candidate,
        Arc::new(SingleResolver {
            plugin: "test.prepare-failure",
            factory,
        }),
    )
    .unwrap();
    let runtime = Runtime::default();
    let baseline = runtime.resource_snapshot();
    let baseline_revision = runtime.snapshot().revision;

    let error = plan
        .activate(&runtime.root(), &CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(
        &error,
        rsi_meta_profile::ProfileError::Preparation { instance }
            if instance.as_str() == "rejected"
    ));
    assert!(!error.to_string().contains("secret"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let after = runtime.resource_snapshot();
    assert_eq!(after.fibers.current, baseline.fibers.current);
    assert_eq!(runtime.snapshot().revision, baseline_revision);
    assert_eq!(after.preparations.current, baseline.preparations.current);
    assert_eq!(
        after.retained_plugin_bytes.current,
        baseline.retained_plugin_bytes.current
    );
    assert_eq!(after.effects.current, baseline.effects.current);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn activation_failure_names_the_exact_leaf_and_rolls_back_earlier_effects() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("owned", "test.effect", Value::Null),
            ProfileEntry::new("failing", "test.activate-failure", Value::Null),
        ])))
        .unwrap();
    let live = Arc::new(AtomicUsize::new(0));
    let plan = ProfileGenerationPlan::resolve(
        candidate,
        Arc::new(ActivationFailureResolver {
            live: Arc::clone(&live),
        }),
    )
    .unwrap();
    let runtime = Runtime::default();
    let baseline = runtime.resource_snapshot();

    let error = plan
        .activate(&runtime.root(), &CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(
        &error,
        rsi_meta_profile::ProfileError::Application { instance }
            if instance.as_str() == "failing"
    ));
    assert!(!error.to_string().contains("secret"));
    assert_eq!(live.load(Ordering::SeqCst), 0);
    let after = runtime.resource_snapshot();
    assert_eq!(after.fibers.current, baseline.fibers.current);
    assert_eq!(after.effects.current, baseline.effects.current);
    assert_eq!(
        after.effect_transactions.current,
        baseline.effect_transactions.current
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn pending_leaf_is_rejected_and_rolls_back_the_wrapper() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("waiting", "test.pending", Value::Null),
        ])))
        .unwrap();
    let factory: Arc<dyn PluginFactory> = Arc::new(PendingFactory);
    let plan = ProfileGenerationPlan::resolve(
        candidate,
        Arc::new(SingleResolver {
            plugin: "test.pending",
            factory,
        }),
    )
    .unwrap();
    let runtime = Runtime::default();
    let baseline = runtime.resource_snapshot();

    let error = plan
        .activate(&runtime.root(), &CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_profile::ProfileError::GenerationPending { instance }
            if instance.as_str() == "waiting"
    ));
    let after = runtime.resource_snapshot();
    assert_eq!(after.fibers.current, baseline.fibers.current);
    assert_eq!(after.preparations.current, baseline.preparations.current);
    assert_eq!(
        after.retained_plugin_bytes.current,
        baseline.retained_plugin_bytes.current
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn cancellation_during_activation_joins_child_rollback_before_returning() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("blocking", "test.blocking", Value::Null),
        ])))
        .unwrap();
    let entered = Arc::new(Notify::new());
    let live = Arc::new(AtomicUsize::new(0));
    let factory: Arc<dyn PluginFactory> = Arc::new(BlockingFactory {
        entered: Arc::clone(&entered),
        live: Arc::clone(&live),
    });
    let plan = ProfileGenerationPlan::resolve(
        candidate,
        Arc::new(SingleResolver {
            plugin: "test.blocking",
            factory,
        }),
    )
    .unwrap();
    let runtime = Runtime::default();
    let baseline = runtime.resource_snapshot();
    let cancellation = CancellationToken::new();
    let activation = tokio::spawn({
        let parent = runtime.root();
        let cancellation = cancellation.clone();
        async move { plan.activate(&parent, &cancellation).await }
    });

    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("blocking activation should start");
    assert_eq!(live.load(Ordering::SeqCst), 1);
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), activation)
        .await
        .expect("cancellation should join generation rollback")
        .expect("activation task should not panic")
        .unwrap_err();

    assert!(matches!(
        error,
        rsi_meta_profile::ProfileError::Meta(rsi_meta::MetaError::Cancelled)
    ));
    assert_eq!(live.load(Ordering::SeqCst), 0);
    let after = runtime.resource_snapshot();
    assert_eq!(after.fibers.current, baseline.fibers.current);
    assert_eq!(after.effects.current, baseline.effects.current);
    assert_eq!(
        after.effect_transactions.current,
        baseline.effect_transactions.current
    );
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn cancellation_waits_for_in_flight_preparation_before_returning() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = compiler(temp.path())
        .compile(&ProfileProgram::from_profile(Profile::new([
            ProfileEntry::new("preparing", "test.blocking-prepare", Value::Null),
        ])))
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let factory: Arc<dyn PluginFactory> = Arc::new(BlockingPreparationFactory {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let plan = ProfileGenerationPlan::resolve(
        candidate,
        Arc::new(SingleResolver {
            plugin: "test.blocking-prepare",
            factory,
        }),
    )
    .unwrap();
    let runtime = Runtime::default();
    let baseline = runtime.resource_snapshot();
    let baseline_revision = runtime.snapshot().revision;
    let cancellation = CancellationToken::new();
    let mut activation = tokio::spawn({
        let parent = runtime.root();
        let cancellation = cancellation.clone();
        async move { plan.activate(&parent, &cancellation).await }
    });

    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("blocking preparation should start");
    cancellation.cancel();
    let early = tokio::time::timeout(Duration::from_millis(100), &mut activation).await;
    let returned_before_release = early.is_ok();
    {
        let (released, ready) = &*release;
        *released.lock().expect("preparation release poisoned") = true;
        ready.notify_one();
    }
    let task_result = match early {
        Ok(task_result) => task_result,
        Err(_) => tokio::time::timeout(Duration::from_secs(2), activation)
            .await
            .expect("released preparation should finish cancellation"),
    };
    let error = task_result
        .expect("activation task should not panic")
        .unwrap_err();

    assert!(
        !returned_before_release,
        "cancellation returned while admitted preparation was still running"
    );
    assert!(matches!(
        error,
        rsi_meta_profile::ProfileError::Meta(rsi_meta::MetaError::Cancelled)
    ));
    let after = runtime.resource_snapshot();
    assert_eq!(after.fibers.current, baseline.fibers.current);
    assert_eq!(runtime.snapshot().revision, baseline_revision);
    assert_eq!(after.preparations.current, baseline.preparations.current);
    assert_eq!(
        after.retained_plugin_bytes.current,
        baseline.retained_plugin_bytes.current
    );
    assert!(runtime.shutdown().await.is_clean());
}

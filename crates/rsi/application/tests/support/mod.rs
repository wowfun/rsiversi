use async_trait::async_trait;
use rsi_application::{ApplicationError, ShellContract, ShellFactory};
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
    ResolvedFactory, Runtime, UpdateMode,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(Debug, Default)]
struct Counts {
    active: AtomicUsize,
    entered: AtomicUsize,
}
#[derive(Debug)]
struct Domain;
impl LocalContract for Domain {
    const KEY: &'static str = "fixture.domain";
    type Service = Counts;
}
#[derive(Debug)]
struct Label;
impl LocalContract for Label {
    const KEY: &'static str = "fixture.surface.label";
    type Service = String;
}

#[derive(Debug)]
struct Parent(Arc<Counts>);
#[async_trait]
impl PluginFactory for Parent {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan.context().provide_local::<Domain>(self.0.clone())?;
        plan.defer(
            "withdraw domain",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
#[derive(Debug)]
struct Leaf;
#[async_trait]
impl PluginFactory for Leaf {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let label = desired
            .as_str()
            .filter(|value| *value != "reject")
            .ok_or_else(|| MetaError::InvalidInput("rejected label".into()))?
            .to_owned();
        let bytes = label.len();
        Ok(
            PreparedActivation::with_state(desired.clone(), label, bytes)
                .requiring_local::<Domain>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let label = plan.take_state::<String>()?;
        let counts = plan.local::<Domain>()?;
        counts.entered.fetch_add(1, Ordering::SeqCst);
        counts.active.fetch_add(1, Ordering::SeqCst);
        let fail = label == "cleanup-failure";
        plan.defer(
            "release surface",
            Box::new(move || {
                Box::pin(async move {
                    counts.active.fetch_sub(1, Ordering::SeqCst);
                    if fail {
                        Err("intentional surface cleanup failure".into())
                    } else {
                        Ok(())
                    }
                })
            }),
        )?;
        if label == "blocked" {
            std::future::pending::<()>().await;
        }
        let supply = plan.context().provide_local::<Label>(Arc::new(label))?;
        plan.defer(
            "withdraw label",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
fn profile(label: &str) -> ProfileProgram {
    ProfileProgram::from_profile(Profile::new([ProfileEntry::new(
        "leaf",
        "leaf",
        serde_json::json!(label),
    )]))
}
async fn fixture(
    execution: rsi_meta::Execution,
    maximum: usize,
) -> (Runtime, Arc<Counts>, rsi_meta::FiberHandle) {
    let runtime = Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution).unwrap();
    let counts = Arc::new(Counts::default());
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "domain",
                "test",
                UpdateMode::Replayable,
                Arc::new(Parent(counts.clone())),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let mut catalog = HostBuilder::without_paths("fixture");
    catalog.register_local_contract::<Label>().unwrap();
    catalog
        .register_linked("leaf", "test", UpdateMode::Replayable, Arc::new(Leaf))
        .unwrap();
    let shell = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "shell",
                "test",
                UpdateMode::RestartRequired,
                Arc::new(ShellFactory::new(catalog.build().unwrap())),
            ),
            serde_json::json!({"maximum_surfaces":maximum}),
        )
        .await
        .unwrap();
    (runtime, counts, shell)
}
async fn until(runtime: &Runtime, condition: impl Fn() -> bool) {
    let execution = runtime.execution();
    execution
        .deadline_after(Duration::from_secs(5))
        .timeout(async {
            while !condition() {
                execution.sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("surface condition did not settle");
}

pub async fn session_free_shell_owns_bounded_isolated_profiles_with_shared_domain(
    execution: rsi_meta::Execution,
) {
    let (runtime, counts, fiber) = fixture(execution, 2).await;
    let shell = runtime.root().lookup_local::<ShellContract>().unwrap();
    let first = shell.open(profile("alpha")).await.unwrap();
    let second = shell.open(profile("beta")).await.unwrap();
    assert_eq!(first.lookup_local::<Label>().unwrap().as_str(), "alpha");
    assert_eq!(second.lookup_local::<Label>().unwrap().as_str(), "beta");
    assert!(Arc::ptr_eq(
        &first.lookup_local::<Domain>().unwrap(),
        &counts
    ));
    assert!(Arc::ptr_eq(
        &second.lookup_local::<Domain>().unwrap(),
        &counts
    ));
    assert!(runtime.root().lookup_local::<Label>().is_none());
    assert!(matches!(
        shell.open(profile("excess")).await,
        Err(ApplicationError::Capacity)
    ));
    assert_eq!(counts.active.load(Ordering::SeqCst), 2);
    assert!(first.close().await.unwrap().is_clean());
    assert!(shell.open(profile("reject")).await.is_err());
    assert_eq!(counts.entered.load(Ordering::SeqCst), 2);
    drop(shell.open(profile("abandoned")));
    until(&runtime, || {
        counts.entered.load(Ordering::SeqCst) == 3 && counts.active.load(Ordering::SeqCst) == 1
    })
    .await;
    second.reload().await.unwrap();
    assert_eq!(second.lookup_local::<Label>().unwrap().as_str(), "beta");
    assert!(fiber.dispose().await.is_clean());
    assert_eq!(counts.active.load(Ordering::SeqCst), 0);
    assert!(second.lookup_local::<Label>().is_none());
    assert!(second.close().await.unwrap().is_clean());
    assert!(matches!(
        shell.open(profile("retired")).await,
        Err(ApplicationError::ShuttingDown)
    ));
    assert!(runtime.root().lookup_local::<Domain>().is_some());
    assert!(runtime.shutdown().await.is_clean());
}

pub async fn retirement_cancels_partial_surface_activation_and_drains_its_owner(
    execution: rsi_meta::Execution,
) {
    let (runtime, counts, fiber) = fixture(execution, 1).await;
    let shell = runtime.root().lookup_local::<ShellContract>().unwrap();
    let opening = shell.open(profile("blocked"));
    until(&runtime, || counts.active.load(Ordering::SeqCst) == 1).await;
    let result = runtime
        .execution()
        .deadline_after(Duration::from_secs(5))
        .timeout(fiber.dispose())
        .await
        .unwrap();
    assert!(result.is_clean(), "{result:?}");
    assert!(opening.await.is_err());
    assert_eq!(counts.active.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}

pub async fn abandoned_surface_cleanup_failure_cannot_be_reported_as_clean_shell_shutdown(
    execution: rsi_meta::Execution,
) {
    let (runtime, counts, fiber) = fixture(execution, 1).await;
    let shell = runtime.root().lookup_local::<ShellContract>().unwrap();
    let surface = shell.open(profile("cleanup-failure")).await.unwrap();
    drop(surface);
    until(&runtime, || counts.active.load(Ordering::SeqCst) == 0).await;
    let report = fiber.dispose().await;
    assert!(
        !report.is_clean(),
        "Shell discarded a completed child cleanup failure"
    );
    let _ = runtime.shutdown().await;
}

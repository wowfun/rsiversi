use async_trait::async_trait;
use rsi_host::{
    HostBuilder, Profile, ProfileControlContract, ProfileEntry, ProfileFragment, ProfileProgram,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, FactoryIdentity, FiberState, LocalContract, MetaError,
    PluginFactory, PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_meta_scope::ScopeRoot;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct Counter;
impl LocalContract for Counter {
    const KEY: &'static str = "test.parent.counter";
    type Service = AtomicUsize;
}
#[derive(Debug)]
struct Label;
impl LocalContract for Label {
    const KEY: &'static str = "test.label";
    type Service = String;
}
#[derive(Debug)]
struct Parent(Arc<AtomicUsize>);
#[async_trait]
impl PluginFactory for Parent {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let counter = plan.context().provide_local::<Counter>(self.0.clone())?;
        let label = plan
            .context()
            .provide_local::<Label>(Arc::new("parent".into()))?;
        plan.defer(
            "withdraw parent",
            Box::new(move || {
                Box::pin(async move {
                    drop((counter, label));
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
        if desired.as_str() == Some("fail") {
            return Err(MetaError::InvalidInput(
                "fixture preparation failure".into(),
            ));
        }
        let label = desired
            .as_str()
            .ok_or_else(|| MetaError::InvalidInput("label required".into()))?
            .to_owned();
        let bytes = label.len();
        Ok(
            PreparedActivation::with_state(desired.clone(), label, bytes)
                .requiring_local::<Counter>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let label = plan.take_state::<String>()?;
        let counter = plan.local::<Counter>()?;
        counter.fetch_add(1, Ordering::AcqRel);
        plan.defer(
            "release child",
            Box::new(move || {
                Box::pin(async move {
                    counter.fetch_sub(1, Ordering::AcqRel);
                    Ok(())
                })
            }),
        )?;
        let label = plan.context().provide_local::<Label>(Arc::new(label))?;
        plan.defer(
            "withdraw child label",
            Box::new(move || {
                Box::pin(async move {
                    drop(label);
                    Ok(())
                })
            }),
        )
    }
}
fn host(label: &str) -> rsi_host::Host {
    let mut builder = HostBuilder::without_paths("native");
    builder.register_local_contract::<Label>().unwrap();
    builder
        .register_linked("label", "test", UpdateMode::Replayable, Arc::new(Leaf))
        .unwrap();
    builder
        .register_fragment(ProfileFragment::new(
            "fixture",
            vec![ProfileEntry::new("leaf", "label", serde_json::json!(label))],
        ))
        .unwrap();
    builder.build().unwrap()
}
fn program() -> ProfileProgram {
    ProfileProgram::from_profile(Profile::default())
}

#[tokio::test]
async fn independent_child_profiles_share_parent_runtime_and_dispose_only_their_scope() {
    let runtime = Runtime::default();
    let count = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "parent",
                "test",
                UpdateMode::Replayable,
                Arc::new(Parent(count.clone())),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let scopes = ScopeRoot::new(16).unwrap();
    let mut children = Vec::new();
    for name in ["alpha", "beta"] {
        let scope = scopes.create(&runtime.root()).await.unwrap();
        let host = host(name);
        let before = runtime.snapshot().fibers.len();
        let context = host
            .isolate_local_context(scope.context().meta().clone())
            .unwrap();
        assert_eq!(runtime.snapshot().fibers.len(), before);
        assert!(context.lookup_local::<Label>().is_none());
        assert!(context.lookup_local::<ProfileControlContract>().is_none());
        assert!(Arc::ptr_eq(
            &context.lookup_local::<Counter>().unwrap(),
            &count
        ));
        let bootstrap = host.prepare_in(&runtime, program()).await.unwrap();
        let control = bootstrap.control();
        let profile = context
            .apply(
                ResolvedFactory::linked(
                    "profile",
                    "test",
                    UpdateMode::RestartRequired,
                    bootstrap.factory(),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        assert_eq!(profile.snapshot().state, FiberState::Active);
        assert!(context.lookup_local::<ProfileControlContract>().is_some());
        assert_eq!(context.lookup_local::<Label>().unwrap().as_str(), name);
        children.push((scope, context, control));
    }
    assert_eq!(count.load(Ordering::Acquire), 2);
    assert_eq!(
        runtime.root().lookup_local::<Label>().unwrap().as_str(),
        "parent"
    );
    assert_eq!(
        runtime
            .snapshot()
            .fibers
            .iter()
            .filter(|fiber| matches!(&fiber.factory, FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == "profile"))
            .count(),
        2
    );
    let (first, first_context, first_control) = children.remove(0);
    assert!(first.dispose().await.is_clean());
    assert!(first_context.lookup_local::<Label>().is_none());
    assert!(first_control.reload().await.is_err());
    assert_eq!(count.load(Ordering::Acquire), 1);
    let (second, second_context, second_control) = children.remove(0);
    second_control.reload().await.unwrap();
    assert_eq!(
        second_context.lookup_local::<Label>().unwrap().as_str(),
        "beta"
    );
    assert_eq!(
        runtime.root().lookup_local::<Label>().unwrap().as_str(),
        "parent"
    );
    assert!(second.dispose().await.is_clean());
    assert_eq!(count.load(Ordering::Acquire), 0);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn rejected_child_preparation_preserves_existing_parent_runtime_and_services() {
    let runtime = Runtime::default();
    let count = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "parent",
                "test",
                UpdateMode::Replayable,
                Arc::new(Parent(count.clone())),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let before = runtime.snapshot().fibers.len();
    assert!(host("fail").prepare_in(&runtime, program()).await.is_err());
    assert_eq!(runtime.snapshot().fibers.len(), before);
    assert_eq!(
        runtime.root().lookup_local::<Label>().unwrap().as_str(),
        "parent"
    );
    assert_eq!(count.load(Ordering::Acquire), 0);
    assert!(runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn caller_can_fence_an_uncatalogued_parent_capability_before_child_activation() {
    let runtime = Runtime::default();
    let count = Arc::new(AtomicUsize::new(0));
    runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "parent",
                "test",
                UpdateMode::Replayable,
                Arc::new(Parent(count.clone())),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    let scopes = ScopeRoot::new(1).unwrap();
    let scope = scopes.create(&runtime.root()).await.unwrap();
    let parent = scope
        .context()
        .meta()
        .clone()
        .isolate_local_fresh::<Counter>()
        .unwrap()
        .0;
    let host = host("child");
    let context = host.isolate_local_context(parent).unwrap();
    assert!(context.lookup_local::<Counter>().is_none());
    let bootstrap = host.prepare_in(&runtime, program()).await.unwrap();
    let profile = context
        .apply(
            ResolvedFactory::linked(
                "profile",
                "test",
                UpdateMode::RestartRequired,
                bootstrap.factory(),
            ),
            ConfigValue::Null,
        )
        .await
        .unwrap();
    assert_eq!(profile.snapshot().state, FiberState::Active);
    assert!(context.lookup_local::<Label>().is_none());
    assert!(runtime.snapshot().fibers.iter().any(|fiber| {
        matches!(&fiber.factory, FactoryIdentity::Linked { plugin, .. } if plugin.as_str() == "label")
            && matches!(fiber.state, FiberState::Pending(_))
    }));
    assert_eq!(count.load(Ordering::Acquire), 0);
    assert!(runtime.root().lookup_local::<Counter>().is_some());
    assert!(scope.dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
}

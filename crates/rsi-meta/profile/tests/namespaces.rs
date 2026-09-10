use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, Context, Emit, EmitEventHandler, LocalContract,
    LocalEvent, LocalEventOptions, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_meta_profile::{
    ProfileBundle, ProfileCompiler, ProfileEnvironment, ProfileGenerationPlan, ProfileLimits,
    ProfileProgram, ProfileResolver,
};
use std::any::TypeId;
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

struct Counter;
impl LocalContract for Counter {
    const KEY: &'static str = "counter";
    type Service = AtomicUsize;
}
struct Increment;
impl LocalEvent for Increment {
    const KEY: &'static str = "counter";
    type Value = ();
    type Error = std::convert::Infallible;
    type Mode = Emit;
}
#[derive(Debug)]
struct IncrementCounter(Arc<AtomicUsize>);
impl EmitEventHandler<Increment> for IncrementCounter {
    fn handle(&self, (): &()) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl rsi_meta::ServiceEndpoint for IncrementCounter {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: rsi_meta::ProviderChannel<'_>,
    ) -> rsi_meta::Result<()> {
        while channel.recv().await.is_some() {
            let value = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            channel
                .send(rsi_meta::Message::new(
                    (value as u64).to_le_bytes().as_slice(),
                ))
                .await?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Captured(Context, Arc<AtomicUsize>, Capability);
#[derive(Debug)]
struct Factory(Arc<Mutex<Vec<Captured>>>);
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let prepared = PreparedActivation::new(config.clone());
        Ok(if config["provider"] == true {
            prepared
        } else {
            prepared
                .requiring_local::<Counter>()
                .requiring(rsi_meta::Requirement::new(
                    "counter",
                    "increment",
                    rsi_meta::ContractVersion(1),
                ))
        })
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        if plan.config()["provider"] == true {
            let counter = Arc::new(AtomicUsize::new(0));
            plan.context().provide_local::<Counter>(counter.clone())?;
            plan.context().on_emit::<Increment, _>(
                Arc::new(IncrementCounter(counter.clone())),
                LocalEventOptions::default(),
            )?;
            plan.context().provide(
                "counter",
                "increment",
                rsi_meta::ContractVersion(1),
                Arc::new(IncrementCounter(counter)),
            )?;
        } else {
            self.0.lock().unwrap().push(Captured(
                plan.context().clone(),
                plan.local::<Counter>()?,
                plan.inject("counter").unwrap().clone(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug)]
struct Resolver(Arc<Factory>);
impl ProfileResolver for Resolver {
    fn resolve(&self, plugin: &rsi_meta::PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        Ok(ResolvedFactory::linked(
            plugin.clone(),
            "one",
            UpdateMode::Replayable,
            self.0.clone(),
        ))
    }
    fn local_contract_type(&self, _: &str) -> rsi_meta_profile::Result<TypeId> {
        Ok(TypeId::of::<Counter>())
    }
    fn local_event_type(&self, _: &str) -> rsi_meta_profile::Result<TypeId> {
        Ok(TypeId::of::<Increment>())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn includes_share_named_bindings_but_static_wrappers_never_share_any_lane() {
    namespace_scenario(Runtime::default()).await;
}

/// Public-seam scenario shared with the actual Dedicated Worker fixture.
///
/// # Panics
/// Fails when Profile sharing, namespace separation or cleanup violates the contract.
pub async fn namespace_scenario(runtime: Runtime) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let resolver = Arc::new(Resolver(Arc::new(Factory(captured.clone()))));
    let limits = ProfileLimits::default();
    let group = |id: &str, provider: bool| {
        format!(
            r#"format = 1
[[steps]]
kind = "group"
id = "{id}"
[steps.isolation]
local = [{{ key = "counter", label = "same" }}]
events = [{{ key = "counter", label = "same" }}]
portable = [{{ key = "counter", label = "same" }}]
[[steps.nodes]]
kind = "plugin"
id = "{id}-leaf"
plugin = "factory"
config = {{ provider = {provider} }}
"#
        )
    };
    let bundle = ProfileBundle::new("root.toml", BTreeMap::from([
        ("root.toml".into(), b"format = 1\n[[steps]]\nkind = 'include'\npath = 'provider.toml'\n[[steps]]\nkind = 'include'\npath = 'consumer.toml'\n".to_vec()),
        ("provider.toml".into(), group("provider", true).into_bytes()),
        ("consumer.toml".into(), group("consumer", false).into_bytes()),
    ]), &limits).unwrap();
    let candidate = ProfileCompiler::new(
        ProfileEnvironment::without_paths("test", BTreeMap::new()).unwrap(),
        limits,
    )
    .compile(&ProfileProgram::from_bundle(bundle))
    .unwrap();
    let mut wrappers = Vec::new();
    let mut consumers = Vec::new();
    for _ in 0..2 {
        wrappers.push(
            ProfileGenerationPlan::resolve(candidate.clone(), resolver.clone())
                .unwrap()
                .activate(&runtime.root(), &CancellationToken::new())
                .await
                .unwrap(),
        );
        // Remove captured Contexts from the factory immediately: the test owns
        // them independently of the Runtime, and drops them before shutdown.
        consumers.push(captured.lock().unwrap().pop().unwrap());
    }
    assert!(!Arc::ptr_eq(&consumers[0].1, &consumers[1].1));
    for (index, consumer) in consumers.iter().enumerate() {
        assert_eq!(consumer.1.load(Ordering::SeqCst), 0);
        consumer.0.dispatch_local::<Increment>(()).unwrap();
        assert_eq!(consumer.1.load(Ordering::SeqCst), 1);
        let reply = consumer
            .2
            .invoke(rsi_meta::Message::new(b"increment".as_slice()))
            .await
            .unwrap();
        assert_eq!(reply.as_bytes(), &2_u64.to_le_bytes());
        assert_eq!(
            consumers[1 - index].1.load(Ordering::SeqCst),
            if index == 0 { 0 } else { 2 }
        );
    }
    assert!(wrappers.remove(0).dispose().await.is_clean());
    assert!(
        consumers[0]
            .2
            .invoke(rsi_meta::Message::new(b"retired".as_slice()))
            .await
            .is_err()
    );
    assert!(
        consumers[1]
            .2
            .invoke(rsi_meta::Message::new(b"alive".as_slice()))
            .await
            .is_ok()
    );
    assert_eq!(consumers[1].1.load(Ordering::SeqCst), 3);
    drop(consumers);
    assert!(wrappers.remove(0).dispose().await.is_clean());
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(runtime.resource_snapshot().effects.current, 0);
}

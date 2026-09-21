//! An independent ordinary addon using only public SDK contracts.
use async_trait::async_trait;
use rsi::{AddonScope, StandardAddonBuilder, StandardAddonSet};
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
    UpdateMode,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
pub struct Greeting(pub String);
#[derive(Debug)]
pub struct GreetingContract;
impl LocalContract for GreetingContract {
    const KEY: &'static str = "addon.linked.greeting";
    type Service = Greeting;
}
#[derive(Debug)]
struct GreetingFactory(Arc<AtomicUsize>);
#[async_trait]
impl PluginFactory for GreetingFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let label = config
            .get("label")
            .and_then(ConfigValue::as_str)
            .filter(|label| {
                !label.is_empty() && label.len() <= 128 && !label.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                MetaError::InvalidInput("label requires 1..128 safe UTF-8 bytes".into())
            })?;
        if config.as_object().is_none_or(|fields| fields.len() != 1) {
            return Err(MetaError::InvalidInput("only label is accepted".into()));
        }
        Ok(PreparedActivation::with_state(
            config.clone(),
            label.to_owned(),
            label.len(),
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let label = plan.take_state::<String>()?;
        let live = self.0.clone();
        live.fetch_add(1, Ordering::SeqCst);
        plan.defer(
            "release greeting",
            Box::new(move || {
                Box::pin(async move {
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        plan.context()
            .provide_local::<GreetingContract>(Arc::new(Greeting(label)))?;
        Ok(())
    }
}
pub fn addons(scope: AddonScope, live: Arc<AtomicUsize>) -> rsi_host::Result<StandardAddonSet> {
    let mut addon = StandardAddonBuilder::new("addon.linked");
    addon.register_factory(
        scope,
        "addon.linked.greeting",
        "1",
        UpdateMode::Replayable,
        Arc::new(GreetingFactory(live)),
    )?;
    addon.register_local_contract_at::<GreetingContract>(scope)?;
    addon.describe_factory("addon.linked.greeting", "Bounded greeting service", Some(json!({
        "type":"object", "properties":{"label":{"type":"string"}}, "required":["label"], "additionalProperties":false
    })))?;
    StandardAddonSet::new([addon.build()?])
}
pub fn program(label: &str) -> ProfileProgram {
    ProfileProgram::from_profile(Profile::new([ProfileEntry::new(
        "greeting",
        "addon.linked.greeting",
        json!({"label":label}),
    )]))
}
pub fn host(scope: AddonScope, live: Arc<AtomicUsize>) -> rsi_host::Result<rsi_host::Host> {
    let mut builder = HostBuilder::without_paths(std::env::consts::OS);
    addons(scope, live)?.register_into(&mut builder, scope)?;
    builder.build()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn public_roles_invoke_replace_and_release_exact_local_generations() {
        for scope in [
            AddonScope::Service,
            AddonScope::Agent,
            AddonScope::Application,
            AddonScope::Client,
        ] {
            let live = Arc::new(AtomicUsize::new(0));
            let first = host(scope, live.clone()).unwrap();
            let next = host(scope, live.clone()).unwrap();
            assert_eq!(live.load(Ordering::SeqCst), 0);
            let running = first.start_program(program("first")).await.unwrap();
            let old = running.lookup_local::<GreetingContract>().unwrap();
            assert_eq!(old.0, "first");
            let update = running
                .updater()
                .submit(1, next.profile_input(program("second")).unwrap())
                .unwrap();
            update.wait().await.unwrap();
            assert_eq!(
                running.lookup_local::<GreetingContract>().unwrap().0,
                "second"
            );
            assert_eq!(old.0, "first");
            assert_eq!(live.load(Ordering::SeqCst), 1);
            assert!(running.shutdown().await.is_clean());
            assert_eq!(live.load(Ordering::SeqCst), 0);
        }
    }
}

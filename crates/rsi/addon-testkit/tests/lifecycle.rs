use async_trait::async_trait;
use rsi::{AddonScope, StandardAddonBuilder, StandardAddonSet};
use rsi_addon_testkit::{GenerationProbe, assert_addon_generations};
use rsi_host::{Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, PluginFactory, PreparedActivation, UpdateMode,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
struct Contract;
impl LocalContract for Contract {
    const KEY: &'static str = "fixture.addon.value";
    type Service = u64;
}
#[derive(Debug)]
struct Factory(Arc<AtomicUsize>);
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let value = desired
            .as_u64()
            .ok_or_else(|| rsi_meta::MetaError::InvalidInput("integer required".into()))?;
        Ok(PreparedActivation::with_state(desired.clone(), value, 8))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let value = plan.take_state::<u64>()?;
        self.0.fetch_add(1, Ordering::SeqCst);
        let count = self.0.clone();
        plan.defer(
            "drop fixture generation",
            Box::new(move || {
                Box::pin(async move {
                    count.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        )?;
        plan.context().provide_local::<Contract>(Arc::new(value))?;
        Ok(())
    }
}
#[tokio::test]
async fn all_roles_probe_actual_exports_and_leave_no_active_generation() {
    for role in [
        AddonScope::Service,
        AddonScope::Agent,
        AddonScope::Application,
        AddonScope::Client,
    ] {
        let active = Arc::new(AtomicUsize::new(0));
        let mut builder = StandardAddonBuilder::new("fixture.addon");
        builder
            .register_factory(
                role,
                "fixture.addon.value",
                "1",
                UpdateMode::Replayable,
                Arc::new(Factory(active.clone())),
            )
            .unwrap();
        builder
            .register_local_contract_at::<Contract>(role)
            .unwrap();
        let addons = StandardAddonSet::new([builder.build().unwrap()]).unwrap();
        let program = |value| {
            ProfileProgram::from_profile(Profile::new([ProfileEntry::new(
                "value",
                "fixture.addon.value",
                serde_json::json!(value),
            )]))
        };
        let probes = AtomicUsize::new(0);
        assert_addon_generations::<Contract>(
            &addons,
            role,
            [program(1), program(2)],
            |stage, value| {
                probes.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    *value,
                    if stage == GenerationProbe::Replacement {
                        2
                    } else {
                        1
                    }
                );
                assert_eq!(active.load(Ordering::SeqCst), 1);
            },
        )
        .await;
        assert_eq!(probes.load(Ordering::SeqCst), 3);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}

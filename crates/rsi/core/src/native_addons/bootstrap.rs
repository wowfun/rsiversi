use crate::StandardComposition;
use rsi_application::ScopedProfile;
use rsi_host::{HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, UpdateMode};
use std::sync::Arc;

pub(crate) fn deferred(
    composition: &StandardComposition,
    scope: crate::AddonScope,
) -> crate::Result<std::collections::BTreeSet<rsi_meta::PluginId>> {
    let root = rsi_files_native_fs::resolve_absolute_root_alias(
        &composition.paths().config().join("native-addons"),
        true,
    )
    .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
    let selection = crate::NativeAddonStore::read_snapshot(&root)
        .map_err(|error| crate::RsiError::Boot(error.to_string()))?;
    Ok(selection
        .into_iter()
        .flat_map(|snapshot| snapshot.enabled)
        .filter(|record| record.scope() == scope)
        .map(|record| rsi_meta::PluginId::new(record.plugin()))
        .collect())
}

/// Staging is a normal child Profile; both product entry paths use this owner.
pub(crate) async fn stage(
    composition: &StandardComposition,
    plan: &mut ActivationPlan,
    service: bool,
) -> rsi_meta::Result<Arc<ScopedProfile>> {
    let factory = plan
        .context()
        .runtime()
        .execution()
        .prepare({
            let composition = composition.clone();
            move || composition.native_bootstrap_factory(service)
        })
        .await
        .map_err(activation)?
        .map_err(activation)?;
    let mut builder = HostBuilder::new(composition.paths().clone());
    builder
        .register_local_contract::<super::NativeStagingContract>()
        .map_err(activation)?;
    builder
        .register_local_contract::<super::NativeAddonControlContract>()
        .map_err(activation)?;
    builder
        .register_local_contract::<rsi_agent_composition::AgentCompositionSourceContract>()
        .map_err(activation)?;
    let mut entries = Vec::new();
    if service {
        builder
            .register_local_contract::<rsi_service_host::ServiceOwnerContract>()
            .map_err(activation)?;
        builder
            .register_local_contract::<rsi_api_protocol::HostGenerationContract>()
            .map_err(activation)?;
        builder
            .register_linked(
                "rsi.service.owner",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(composition.service_owner_factory().map_err(activation)?),
            )
            .map_err(activation)?;
        entries.push(ProfileEntry::new(
            "owner",
            "rsi.service.owner",
            ConfigValue::Null,
        ));
    }
    builder
        .register_linked(
            "rsi.native-addons",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(factory),
        )
        .map_err(activation)?;
    entries.push(ProfileEntry::new(
        "native",
        "rsi.native-addons",
        ConfigValue::Null,
    ));
    let native = Arc::new(
        ScopedProfile::start(
            &builder.build().map_err(activation)?,
            plan.context(),
            ProfileProgram::from_profile(Profile::new(entries)),
        )
        .await
        .map_err(activation)?,
    );
    let cleanup = native.clone();
    plan.defer(
        "close bootstrap native staging",
        Box::new(move || {
            Box::pin(async move {
                if cleanup.shutdown().await.is_clean() {
                    Ok(())
                } else {
                    Err("native staging cleanup failed".into())
                }
            })
        }),
    )?;
    Ok(native)
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

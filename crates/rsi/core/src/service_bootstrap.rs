use crate::{ProfileOwner, Result, RsiError, StandardComposition};
use async_trait::async_trait;
use rsi_application::ScopedProfile;
use rsi_host::{Host, HostBuilder, Profile, ProfileEntry, ProfileProgram};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, LocalContract, MetaError, PluginFactory,
    PreparedActivation, UpdateMode,
};
use std::sync::Arc;

#[derive(Debug)]
struct ServiceProfileContract;
impl LocalContract for ServiceProfileContract {
    const KEY: &'static str = "rsi.service.bootstrap.profile";
    type Service = ScopedProfile;
}

pub(crate) async fn start(
    composition: StandardComposition,
    program: ProfileProgram,
    launch_key: Option<String>,
    parent: Option<&Context>,
) -> Result<ProfileOwner> {
    composition.preflight_service(program.clone(), launch_key.as_deref())?;
    if let Some(source) = composition.service_catalog_source(launch_key.clone()) {
        let parent =
            parent.ok_or_else(|| boot("a staged composition must remain in its owning Runtime"))?;
        let mut owner = ProfileOwner::start_scoped(
            build(&composition, launch_key.as_deref())?,
            composition.paths().clone(),
            parent,
            program.clone(),
        )
        .await?;
        owner.follow_catalog(source, program)?;
        return Ok(owner);
    }
    let paths = composition.paths().clone();
    let mut builder = HostBuilder::new(paths.clone());
    builder
        .register_local_contract::<ServiceProfileContract>()
        .map_err(boot)?;
    builder
        .register_linked(
            "rsi.service.bootstrap",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(Bootstrap {
                composition,
                program,
                launch_key,
            }),
        )
        .map_err(boot)?;
    let host = builder.build().map_err(boot)?;
    let program = ProfileProgram::from_profile(Profile::new(vec![ProfileEntry::new(
        "bootstrap",
        "rsi.service.bootstrap",
        ConfigValue::Null,
    )]));
    let owner = match parent {
        Some(parent) => ProfileOwner::start_scoped(host, paths, parent, program).await?,
        None => ProfileOwner::Root(host.start_program(program).await.map_err(boot)?),
    };
    let Some(profile) = owner.lookup_local::<ServiceProfileContract>() else {
        let _ = owner.shutdown().await;
        return Err(boot("Service bootstrap did not publish its child Profile"));
    };
    Ok(ProfileOwner::Product {
        owner: Box::new(owner),
        profile,
    })
}

#[derive(Debug)]
struct Bootstrap {
    composition: StandardComposition,
    program: ProfileProgram,
    launch_key: Option<String>,
}
#[async_trait]
impl PluginFactory for Bootstrap {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(activation("Service bootstrap configuration must be null"));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let native =
            crate::native_addons::bootstrap::stage(&self.composition, &mut plan, true).await?;
        let staging = native
            .lookup_local::<crate::native_addons::NativeStagingContract>()
            .ok_or_else(|| activation("native staging did not publish its source"))?;
        let owner = native
            .lookup_local::<rsi_service_host::ServiceOwnerContract>()
            .ok_or_else(|| activation("Service Owner was not acquired"))?;
        let epoch = native
            .lookup_local::<rsi_api_protocol::HostGenerationContract>()
            .ok_or_else(|| activation("Service epoch was not published"))?;
        let composition = self
            .composition
            .clone()
            .with_service_owner(owner, epoch.as_ref().clone())
            .map_err(activation)?
            .with_native_staging(staging.as_ref().clone())
            .map_err(activation)?;
        let source = composition
            .service_catalog_source(self.launch_key.clone())
            .expect("staged composition");
        // Actual activation owns builtin preset materialization; catalog snapshots
        // and linked preflight remain free of those writes.
        let mut profile = ScopedProfile::start(
            &build(&composition, self.launch_key.as_deref()).map_err(activation)?,
            plan.context(),
            self.program.clone(),
        )
        .await
        .map_err(activation)?;
        profile
            .follow_catalog(source, self.program.clone())
            .map_err(activation)?;
        let profile = Arc::new(profile);
        let cleanup = profile.clone();
        plan.defer(
            "close Service child Profile",
            Box::new(move || {
                Box::pin(async move {
                    if cleanup.shutdown().await.is_clean() {
                        Ok(())
                    } else {
                        Err("Service Profile cleanup failed".into())
                    }
                })
            }),
        )?;
        plan.context()
            .provide_local::<ServiceProfileContract>(profile)?;
        Ok(())
    }
}
fn build(composition: &StandardComposition, launch_key: Option<&str>) -> Result<Host> {
    #[cfg(target_os = "linux")]
    if let Some(key) = launch_key {
        return composition.clone().build_daemon(key).map_err(boot);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = launch_key;
    composition.clone().build().map_err(boot)
}
fn boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

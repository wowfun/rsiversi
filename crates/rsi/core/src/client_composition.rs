pub(crate) const HTTP_PLUGIN: &str = "rsi.connection.http";
#[cfg(any(target_os = "linux", windows))]
pub(crate) const LOCAL_PLUGIN: &str = "rsi.connection.local";

#[cfg(any(target_os = "linux", windows))]
use crate::{Result, RsiError};
#[cfg(any(target_os = "linux", windows))]
use rsi_api_uds_client::{UdsClient, UdsClientConfig, UdsClientFactory};
#[cfg(any(target_os = "linux", windows))]
use rsi_host::ProfileEntry;
#[cfg(any(target_os = "linux", windows))]
use rsi_host::{Profile, ProfileProgram};
#[cfg(any(target_os = "linux", windows))]
use rsi_meta::UpdateMode;
#[cfg(any(target_os = "linux", windows))]
use rsi_service_host::{HostOwnerMetadata, HostOwnerMode, local_compatibility_key};
#[cfg(any(unix, windows))]
use std::sync::Arc;

#[cfg(any(target_os = "linux", windows))]
pub(crate) fn configuration(owner: &HostOwnerMetadata) -> Result<UdsClientConfig> {
    if !owner.is_compatible_with_current().map_err(error)? || owner.mode != HostOwnerMode::Daemon {
        return Err(RsiError::Boot(
            "owner does not describe a compatible daemon API".into(),
        ));
    }
    Ok(UdsClientConfig {
        socket: owner
            .socket_path
            .clone()
            .ok_or_else(|| RsiError::Boot("daemon has no endpoint".into()))?,
        endpoint_id: owner
            .endpoint_id
            .clone()
            .ok_or_else(|| RsiError::Boot("daemon has no deployment identity".into()))?,
        host_epoch: owner.host_epoch.clone(),
        compatibility: local_compatibility_key(&owner.launch_key).map_err(error)?,
        #[cfg(windows)]
        process_id: owner.pid,
        #[cfg(windows)]
        process_start_token: owner.process_start_token.clone(),
    })
}

#[cfg(any(target_os = "linux", windows))]
pub(crate) async fn connect(
    owner: &HostOwnerMetadata,
    parent: &rsi_meta::Context,
    composition: &crate::StandardComposition,
) -> Result<crate::ProfileOwner> {
    let config = serde_json::to_value(configuration(owner)?).map_err(error)?;
    let program = ProfileProgram::from_profile(Profile::default());
    #[allow(unused_mut)]
    let mut connection = crate::ProfileOwner::start_scoped(
        client_host(composition, &config).map_err(error)?,
        composition.paths().clone(),
        parent,
        program.clone(),
    )
    .await?;
    #[cfg(unix)]
    if let Some(staging) = composition.native_staging() {
        connection.follow_catalog(
            Arc::new(ClientCatalog {
                composition: composition.catalog_base(),
                staging,
                config,
                build: client_host,
            }),
            program,
        )?;
    }
    Ok(connection)
}

#[cfg(any(target_os = "linux", windows))]
fn client_host(
    composition: &crate::StandardComposition,
    config: &rsi_meta::ConfigValue,
) -> rsi_host::Result<rsi_host::Host> {
    let (mut builder, mut entries) = rsi_client_composition::domain_clients("native")?;
    builder.register_linked(
        LOCAL_PLUGIN,
        env!("CARGO_PKG_VERSION"),
        UpdateMode::RestartRequired,
        Arc::new(UdsClientFactory),
    )?;
    entries.insert(
        0,
        ProfileEntry::new("connection", LOCAL_PLUGIN, config.clone()),
    );
    builder.register_fragment(rsi_host::ProfileFragment::new(
        "rsi.standard.clients",
        entries,
    ))?;
    composition
        .addons()
        .register_into(&mut builder, crate::AddonScope::Client)?;
    builder.build()
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct ClientCatalog {
    pub(crate) composition: crate::StandardComposition,
    pub(crate) staging: crate::native_addons::NativeStaging,
    pub(crate) config: rsi_meta::ConfigValue,
    pub(crate) build:
        fn(&crate::StandardComposition, &rsi_meta::ConfigValue) -> rsi_host::Result<rsi_host::Host>,
}
#[cfg(unix)]
impl rsi_application::ProfileCatalogSource for ClientCatalog {
    fn snapshot(&self) -> rsi_host::Result<Arc<rsi_host::Host>> {
        let composition = self
            .composition
            .clone()
            .with_native_staging(self.staging.clone())
            .map_err(|error| rsi_host::HostError::Bootstrap(error.to_string()))?;
        (self.build)(&composition, &self.config).map(Arc::new)
    }
    fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.staging.manager.changes()
    }
}

/// Reads Local bounded restart impact without granting shutdown or configuration authority.
#[cfg(any(target_os = "linux", windows))]
pub async fn workflow_restart_impact(
    owner: &HostOwnerMetadata,
) -> Result<rsi_inspector::WorkflowImpact> {
    let client = Arc::new(
        UdsClient::connect(
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            configuration(owner)?,
        )
        .await
        .map_err(error)?,
    );
    let result = rsi_inspector::InspectorClient::new(client.clone())
        .workflows()
        .await
        .map_err(error);
    client.close().await;
    result
}

/// Negotiates the exact current local API and retires the short-lived readiness client.
#[cfg(any(target_os = "linux", windows))]
pub async fn probe_service_host(owner: &HostOwnerMetadata) -> Result<()> {
    let client = UdsClient::connect(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        configuration(owner)?,
    )
    .await
    .map_err(error)?;
    client.close().await;
    Ok(())
}

#[cfg(any(target_os = "linux", windows))]
fn error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}

#[cfg(unix)]
pub(crate) fn linked_plugins() -> rsi_host::Result<Vec<String>> {
    let (_, entries) = rsi_client_composition::domain_clients("native")?;
    let mut plugins: Vec<_> = entries
        .iter()
        .map(|entry| entry.plugin().as_str().to_owned())
        .collect();
    plugins.push(HTTP_PLUGIN.into());
    #[cfg(any(target_os = "linux", windows))]
    plugins.push(LOCAL_PLUGIN.into());
    Ok(plugins)
}

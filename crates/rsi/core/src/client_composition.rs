use crate::{Result, RsiError};
#[cfg(target_os = "linux")]
use rsi_api_uds_client::{UdsClient, UdsClientConfig, UdsClientFactory};
#[cfg(target_os = "linux")]
use rsi_host::ProfileEntry;
#[cfg(target_os = "linux")]
use rsi_host::{Profile, ProfileProgram};
#[cfg(target_os = "linux")]
use rsi_meta::UpdateMode;
#[cfg(target_os = "linux")]
use rsi_service_host::{HostOwnerMetadata, HostOwnerMode, local_compatibility_key};
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
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
    })
}

#[cfg(target_os = "linux")]
pub(crate) async fn connect(
    owner: &HostOwnerMetadata,
    parent: &rsi_meta::Context,
    paths: rsi_host::HostPaths,
) -> Result<crate::ProfileOwner> {
    let config = configuration(owner)?;
    let (mut builder, mut entries) =
        rsi_client_composition::domain_clients("native").map_err(error)?;
    builder
        .register_linked(
            "rsi.connection.local",
            env!("CARGO_PKG_VERSION"),
            UpdateMode::RestartRequired,
            Arc::new(UdsClientFactory),
        )
        .map_err(error)?;
    entries.insert(
        0,
        ProfileEntry::new(
            "connection",
            "rsi.connection.local",
            serde_json::to_value(config).map_err(error)?,
        ),
    );
    crate::ProfileOwner::start_scoped(
        builder.build().map_err(error)?,
        paths,
        parent,
        ProfileProgram::from_profile(Profile::new(entries)),
    )
    .await
}

/// Negotiates the exact current local API and retires the short-lived readiness client.
#[cfg(target_os = "linux")]
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

fn error(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}

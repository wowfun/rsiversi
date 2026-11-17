use crate::DeviceRegistry;
use async_trait::async_trait;
use rsi_api_protocol::{
    DeviceAdministrationContract, DeviceAuthenticationContract, EndpointIdentityContract,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage_domain::DomainFacilityContract;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Exact non-session storage route for this deployment's device verifiers.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceAuthConfig {
    /// Named backend supplied through the Storage domain facility.
    pub backend: String,
}
/// Ordinary native owner of device verification and local administration.
#[derive(Clone, Debug, Default)]
pub struct DeviceAuthFactory;
#[async_trait]
impl PluginFactory for DeviceAuthFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: DeviceAuthConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        rsi_storage::validate_identifier("device storage backend", &config.backend)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = config.backend.len();
        Ok(
            PreparedActivation::with_state(desired.clone(), config, bytes)
                .requiring_local::<DomainFacilityContract>()
                .requiring_local::<EndpointIdentityContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<DeviceAuthConfig>()?;
        let endpoint = plan.local::<EndpointIdentityContract>()?;
        let registry = Arc::new(
            DeviceRegistry::open(
                plan.context().runtime().execution().clone(),
                plan.local::<DomainFacilityContract>()?.as_ref(),
                &config.backend,
                (*endpoint).clone(),
            )
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
        let authentication = plan
            .context()
            .provide_local::<DeviceAuthenticationContract>(registry.clone())?;
        let administration = plan
            .context()
            .provide_local::<DeviceAdministrationContract>(registry.clone())?;
        let cleanup = registry;
        plan.defer(
            "revoke authentication generation",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        plan.defer(
            "withdraw device capabilities",
            Box::new(move || {
                Box::pin(async move {
                    drop(administration);
                    drop(authentication);
                    Ok(())
                })
            }),
        )
    }
}

use crate::{HostOwnerLease, ServiceOwnerContract};
use async_trait::async_trait;
use rsi_api_protocol::{EndpointId, EndpointIdentityContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage_domain::{DomainFacility, DomainFacilityContract, DomainSpec, Schema};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const STORAGE_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    backend: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
struct Durable {
    endpoint: EndpointId,
}

/// Ordinary publisher of identities protected by an existing exclusive product owner.
#[derive(Clone, Debug, Default)]
pub struct ServiceIdentityFactory;
#[async_trait]
impl PluginFactory for ServiceIdentityFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        rsi_storage::validate_identifier("identity storage backend", &config.backend)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = std::mem::size_of::<Config>() + config.backend.len();
        Ok(
            PreparedActivation::with_state(desired.clone(), config, bytes)
                .requiring_local::<DomainFacilityContract>()
                .requiring_local::<ServiceOwnerContract>(),
        )
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let facility = plan.local::<DomainFacilityContract>()?;
        let owner = plan.local::<ServiceOwnerContract>()?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = plan.context().runtime().execution().spawn(async move {
            let result = load_endpoint(&owner, facility.as_ref(), &config.backend).await;
            let _ = sender.send(result);
        });
        plan.defer(
            "drain identity publication",
            Box::new(move || {
                Box::pin(async move {
                    task.await
                        .map_err(|_| "identity publication task failed".to_owned())?;
                    Ok(())
                })
            }),
        )?;
        let endpoint = receiver
            .await
            .map_err(|_| MetaError::Activation("identity publication was lost".into()))??;
        let endpoint = plan
            .context()
            .provide_local::<EndpointIdentityContract>(Arc::new(endpoint))?;
        plan.defer(
            "withdraw service identities",
            Box::new(move || {
                Box::pin(async move {
                    drop(endpoint);
                    Ok(())
                })
            }),
        )
    }
}

async fn load_endpoint(
    owner: &HostOwnerLease,
    facility: &dyn DomainFacility,
    backend: &str,
) -> rsi_meta::Result<EndpointId> {
    let mut pinned = owner.endpoint.lock().await;
    if pinned.as_ref().is_some_and(|(route, _)| route != backend) {
        return Err(MetaError::Activation(
            "service identity cannot switch storage backend in a running owner".into(),
        ));
    }
    let domain = facility
        .open(
            DomainSpec {
                id: "rsi.service.identity".into(),
                backend: backend.into(),
                version: STORAGE_VERSION,
                maximum_records: 1,
                maximum_bytes: 128,
                ..DomainSpec::default()
            },
            Schema::default()
                .global::<Durable>()
                .map_err(|e| MetaError::Activation(e.to_string()))?,
        )
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?;
    let result = async {
        let global = domain
            .global::<Durable>()
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        let endpoint = if let Some(stored) = global
            .get()
            .map_err(|e| MetaError::Activation(e.to_string()))?
        {
            if pinned
                .as_ref()
                .is_some_and(|(_, expected)| expected != &stored.endpoint)
            {
                return Err(MetaError::Activation(
                    "persisted service identity changed under its active owner".into(),
                ));
            }
            stored.endpoint.clone()
        } else {
            let endpoint = match pinned.as_ref() {
                Some((_, endpoint)) => endpoint.clone(),
                None => EndpointId::generate()
                    .map_err(|error| MetaError::Activation(error.to_string()))?,
            };
            global
                .set(Durable {
                    endpoint: endpoint.clone(),
                })
                .await
                .map_err(|e| MetaError::Activation(e.to_string()))?;
            endpoint
        };
        *pinned = Some((backend.into(), endpoint.clone()));
        Ok(endpoint)
    }
    .await;
    domain.close().await;
    result
}

#[cfg(test)]
rsi_persistence_testkit::payload_contract!(
    persistence,
    Durable,
    STORAGE_VERSION,
    format!("tests/fixtures/persistence/global-v{STORAGE_VERSION}.json"),
    vec![Durable {
        endpoint: EndpointId::from_bytes([7; 16]),
    }]
);

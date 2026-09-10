use crate::{HostOwnerLease, ServiceOwnerContract};
use async_trait::async_trait;
use rsi_api_protocol::{EndpointId, EndpointIdentityContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_storage_domain::{DomainFacility, DomainFacilityContract, DomainSpec};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    backend: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
        .open(DomainSpec {
            id: "rsi.service.identity".into(),
            backend: backend.into(),
            version: 1,
            maximum_records: 1,
            maximum_bytes: 128,
        })
        .await
        .map_err(|error| MetaError::Activation(error.to_string()))?;
    let mut snapshot = domain.snapshot().await;
    if snapshot.len() > 1 || snapshot.keys().any(|key| key != "deployment") {
        return Err(MetaError::Activation(
            "service identity contains unexpected records".into(),
        ));
    }
    let endpoint = if let Some(value) = snapshot.remove("deployment") {
        let stored: Durable = serde_json::from_value(value)
            .map_err(|_| MetaError::Activation("service identity is malformed".into()))?;
        if pinned
            .as_ref()
            .is_some_and(|(_, expected)| expected != &stored.endpoint)
        {
            return Err(MetaError::Activation(
                "persisted service identity changed under its active owner".into(),
            ));
        }
        stored.endpoint
    } else {
        let endpoint = match pinned.as_ref() {
            Some((_, endpoint)) => endpoint.clone(),
            None => {
                EndpointId::generate().map_err(|error| MetaError::Activation(error.to_string()))?
            }
        };
        let value = serde_json::to_value(Durable {
            endpoint: endpoint.clone(),
        })
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        domain
            .put("deployment", value)
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        endpoint
    };
    *pinned = Some((backend.into(), endpoint.clone()));
    Ok(endpoint)
}

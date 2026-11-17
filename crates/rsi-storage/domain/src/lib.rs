//! Typed business domains over owned storage units.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod api;
mod facility;
mod handles;
mod queue;
mod schema;
pub use api::{Domain, DomainFacility, DomainFacilityContract, DomainSpec};
pub use handles::{Global, Table, TableSnapshot};
pub use rsi_storage::StorageError;
pub use schema::{Change, ChangeEvent, ChangeOperation, Record, Schema, Snapshot};

use async_trait::async_trait;
use facility::Facility;
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use rsi_storage::StorageHubContract;
use std::sync::Arc;

/// Ordinary plugin factory for the typed domain facility.
#[derive(Clone, Debug, Default)]
pub struct DomainFactory;

#[async_trait]
impl PluginFactory for DomainFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(rsi_meta::MetaError::InvalidInput(
                "storage domain configuration must be null or empty".into(),
            ));
        }
        Ok(
            PreparedActivation::new(serde_json::Value::Null)
                .requiring_local::<StorageHubContract>(),
        )
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let facility = Arc::new(Facility {
            hub: plan.local::<StorageHubContract>()?,
            registry: Arc::default(),
        });
        let supply = plan
            .context()
            .provide_local::<DomainFacilityContract>(facility.clone())?;
        let registration = FacilityRegistration(facility);
        plan.defer(
            "close storage domains and withdraw facility",
            Box::new(move || {
                Box::pin(async move {
                    registration.close().await;
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

struct FacilityRegistration(Arc<Facility>);
impl FacilityRegistration {
    async fn close(self) {
        let owners = self.0.begin_close();
        for owner in owners {
            if owner.loaded.get().is_some() {
                owner.drained().await;
            }
        }
    }
}
impl Drop for FacilityRegistration {
    fn drop(&mut self) {
        self.0.begin_close();
    }
}

#[cfg(test)]
mod tests;

/// Projects storage failures into the shared API taxonomy without losing commit certainty.
pub fn storage_error(error: StorageError) -> rsi_api_protocol::ApiError {
    use rsi_api_protocol::ApiError;
    match error {
        StorageError::Closed => ApiError::ShuttingDown,
        StorageError::OutcomeUnknown(_) => ApiError::OutcomeUnknown,
        StorageError::Io(_)
        | StorageError::RecoveryRequired
        | StorageError::Unsupported(_)
        | StorageError::BackendUnavailable(_) => ApiError::Unavailable,
        StorageError::InvalidInput(message) => ApiError::Invalid(message),
        error @ (StorageError::DuplicateBackend(_)
        | StorageError::Corrupt(_)
        | StorageError::DomainVersionMismatch { .. }) => ApiError::Backend(error.to_string()),
    }
}

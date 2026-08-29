//! Deterministic in-memory Credentials plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_credentials_protocol::{
    CredentialRef, CredentialSource, CredentialsAdmin, CredentialsAdminContract, CredentialsError,
    CredentialsResolve, CredentialsResolveContract, ResolvedCredential, Result, SecretValue,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct MemoryCredentials {
    values: Mutex<HashMap<CredentialRef, SecretValue>>,
}

#[async_trait]
impl CredentialsResolve for MemoryCredentials {
    async fn resolve(&self, reference: &CredentialRef) -> Result<ResolvedCredential> {
        reference.validate()?;
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(reference)
            .cloned()
            .map(|secret| ResolvedCredential {
                secret,
                source: CredentialSource::Keyring,
            })
            .ok_or_else(|| CredentialsError::NotConfigured(reference.account()))
    }
}

#[async_trait]
impl CredentialsAdmin for MemoryCredentials {
    async fn set(&self, reference: &CredentialRef, secret: SecretValue) -> Result<()> {
        reference.validate()?;
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(reference.clone(), secret);
        Ok(())
    }

    async fn unset(&self, reference: &CredentialRef) -> Result<bool> {
        reference.validate()?;
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(reference)
            .is_some())
    }
}

/// Factory for an empty writable memory Credentials service.
#[derive(Clone, Debug, Default)]
pub struct MemoryCredentialsFactory;

#[async_trait]
impl PluginFactory for MemoryCredentialsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "memory Credentials configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service = Arc::new(MemoryCredentials {
            values: Mutex::new(HashMap::new()),
        });
        let resolve: Arc<dyn CredentialsResolve> = service.clone();
        let admin: Arc<dyn CredentialsAdmin> = service;
        let resolve_supply = plan
            .context()
            .provide_local::<CredentialsResolveContract>(resolve)?;
        let admin_supply = plan
            .context()
            .provide_local::<CredentialsAdminContract>(admin)?;
        plan.defer(
            "withdraw memory credential services",
            Box::new(move || {
                Box::pin(async move {
                    drop(admin_supply);
                    drop(resolve_supply);
                    Ok(())
                })
            }),
        )
    }
}

//! Private-file and captured-environment credential provider.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_credentials_protocol::{
    CredentialAvailability, CredentialRef, CredentialSource, CredentialStatus,
    CredentialStoreFailure, CredentialsAdmin, CredentialsAdminContract, CredentialsError,
    CredentialsResolve, CredentialsResolveContract, CredentialsStatus, CredentialsStatusContract,
    ResolvedCredential, Result, SecretValue, validate_environment_name,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Semaphore, watch};

const DEFAULT_MAXIMUM_CONCURRENT_STORE_OPERATIONS: usize = 8;
const MAXIMUM_CONCURRENT_STORE_OPERATIONS: usize = 64;
const DEFAULT_RESOLUTION_TIMEOUT_MS: u64 = 30_000;
const MAXIMUM_RESOLUTION_TIMEOUT_MS: u64 = 5 * 60 * 1_000;

mod file;
pub use file::FileSecretStore;

/// Exact-reference secret-store seam used by the local provider.
pub trait SecretStore: fmt::Debug + Send + Sync + 'static {
    /// Reads one full owner/slot reference.
    fn get(&self, reference: &CredentialRef) -> Result<Option<SecretValue>>;
    /// Replaces one entry independently of other configuration.
    fn set(&self, reference: &CredentialRef, secret: &SecretValue) -> Result<()>;
    /// Deletes a stored entry, allowing environment fallback again.
    fn unset(&self, reference: &CredentialRef) -> Result<bool>;
    /// Safe bounded filesystem location, absent for non-file test stores.
    fn location(&self) -> Option<String> {
        None
    }
}

/// Non-secret mapping from one credential reference to an allowed variable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentBinding {
    /// Exact credential address.
    pub reference: CredentialRef,
    /// Allowed startup variable name.
    pub variable: String,
}

/// Configuration accepted by [`CredentialsLocalFactory`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialsLocalConfig {
    /// Explicit environment fallback mapping.
    #[serde(default)]
    pub environment: Vec<EnvironmentBinding>,
    /// Maximum synchronous secret-store operations admitted concurrently.
    #[serde(default = "default_maximum_concurrent_store_operations")]
    pub maximum_concurrent_store_operations: usize,
    /// Per-waiter credential resolution deadline in milliseconds.
    #[serde(default = "default_resolution_timeout_ms")]
    pub resolution_timeout_ms: u64,
}

const fn default_maximum_concurrent_store_operations() -> usize {
    DEFAULT_MAXIMUM_CONCURRENT_STORE_OPERATIONS
}

const fn default_resolution_timeout_ms() -> u64 {
    DEFAULT_RESOLUTION_TIMEOUT_MS
}

impl CredentialsLocalConfig {
    fn validate(&self) -> Result<()> {
        if self.maximum_concurrent_store_operations == 0
            || self.maximum_concurrent_store_operations > MAXIMUM_CONCURRENT_STORE_OPERATIONS
        {
            return Err(CredentialsError::InvalidInput(format!(
                "maximum_concurrent_store_operations must be within 1..={MAXIMUM_CONCURRENT_STORE_OPERATIONS}"
            )));
        }
        if self.resolution_timeout_ms == 0
            || self.resolution_timeout_ms > MAXIMUM_RESOLUTION_TIMEOUT_MS
        {
            return Err(CredentialsError::InvalidInput(format!(
                "resolution_timeout_ms must be within 1..={MAXIMUM_RESOLUTION_TIMEOUT_MS}"
            )));
        }
        let mut references = HashSet::new();
        for binding in &self.environment {
            binding.reference.validate()?;
            validate_environment_name(&binding.variable)?;
            if !references.insert(binding.reference.clone()) {
                return Err(CredentialsError::InvalidInput(format!(
                    "duplicate environment binding for {}",
                    binding.reference.account()
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Service {
    store: Arc<dyn SecretStore>,
    environment: HashMap<CredentialRef, EnvironmentValue>,
    flights: Arc<Mutex<HashMap<CredentialRef, ResolutionFlight>>>,
    admission: Arc<Semaphore>,
    resolution_timeout: Duration,
}

type ResolutionFlight = watch::Sender<Option<Result<ResolvedCredential>>>;

#[derive(Clone, Debug)]
struct EnvironmentValue {
    variable: String,
    secret: SecretValue,
}

#[async_trait]
impl CredentialsResolve for Service {
    async fn resolve(&self, reference: &CredentialRef) -> Result<ResolvedCredential> {
        reference.validate()?;
        tokio::time::timeout(self.resolution_timeout, self.resolve_admitted(reference))
            .await
            .map_err(|_| CredentialsError::Timeout(reference.account()))?
    }
}

#[async_trait]
impl CredentialsStatus for Service {
    async fn status(&self, reference: &CredentialRef) -> Result<CredentialStatus> {
        reference.validate()?;
        let availability = match self.resolve(reference).await {
            Ok(resolved) => CredentialAvailability::Configured {
                source: resolved.source,
            },
            Err(CredentialsError::NotConfigured(_)) => CredentialAvailability::Missing,
            Err(CredentialsError::Store(reason)) => CredentialAvailability::Unavailable { reason },
            Err(CredentialsError::Timeout(_)) => CredentialAvailability::Unavailable {
                reason: CredentialStoreFailure::Timeout,
            },
            Err(error) => return Err(error),
        };
        let editable = !matches!(availability, CredentialAvailability::Unavailable { .. });
        Ok(CredentialStatus {
            availability,
            editable,
            store_path: self.store.location(),
        })
    }
}

impl Service {
    async fn resolve_admitted(&self, reference: &CredentialRef) -> Result<ResolvedCredential> {
        let existing = {
            let flights = self
                .flights
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            flights.get(reference).map(watch::Sender::subscribe)
        };
        if let Some(receiver) = existing {
            return wait_for_resolution(receiver).await;
        }

        let permit = Arc::clone(&self.admission)
            .acquire_owned()
            .await
            .map_err(|_| CredentialsError::Store(CredentialStoreFailure::Io))?;
        let receiver = {
            let mut flights = self
                .flights
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(flight) = flights.get(reference) {
                drop(permit);
                flight.subscribe()
            } else {
                let (sender, receiver) = watch::channel(None);
                flights.insert(reference.clone(), sender.clone());
                let store = Arc::clone(&self.store);
                let reference = reference.clone();
                let environment = self.environment.get(&reference).cloned();
                let flights = Arc::clone(&self.flights);
                tokio::spawn(async move {
                    let _permit = permit;
                    let lookup = reference.clone();
                    let stored = tokio::task::spawn_blocking(move || store.get(&lookup))
                        .await
                        .map_err(|_| CredentialsError::Store(CredentialStoreFailure::Io))
                        .and_then(std::convert::identity);
                    let result = resolve_stored(&reference, environment, stored);
                    let _ignored = sender.send(Some(result));
                    let mut flights = flights
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if flights
                        .get(&reference)
                        .is_some_and(|current| current.same_channel(&sender))
                    {
                        flights.remove(&reference);
                    }
                });
                receiver
            }
        };
        wait_for_resolution(receiver).await
    }
}

async fn wait_for_resolution(
    mut receiver: watch::Receiver<Option<Result<ResolvedCredential>>>,
) -> Result<ResolvedCredential> {
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result;
        }
        if receiver.changed().await.is_err() {
            return Err(CredentialsError::Store(CredentialStoreFailure::Io));
        }
    }
}

fn resolve_stored(
    reference: &CredentialRef,
    environment: Option<EnvironmentValue>,
    stored: Result<Option<SecretValue>>,
) -> Result<ResolvedCredential> {
    match stored {
        Ok(Some(secret)) => {
            return Ok(ResolvedCredential {
                secret,
                source: CredentialSource::File,
            });
        }
        Ok(None) => {}
        Err(error) => return Err(error),
    }
    if let Some(value) = environment {
        return Ok(ResolvedCredential {
            secret: value.secret,
            source: CredentialSource::Environment {
                variable: value.variable,
            },
        });
    }
    Err(CredentialsError::NotConfigured(reference.account()))
}

struct WriteInvalidation {
    reference: CredentialRef,
    flights: Arc<Mutex<HashMap<CredentialRef, ResolutionFlight>>>,
}
impl Drop for WriteInvalidation {
    fn drop(&mut self) {
        self.flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.reference);
    }
}

#[async_trait]
impl CredentialsAdmin for Service {
    async fn set(&self, reference: &CredentialRef, secret: SecretValue) -> Result<()> {
        reference.validate()?;
        let store = Arc::clone(&self.store);
        let flights = Arc::clone(&self.flights);
        let reference = reference.clone();
        let permit = Arc::clone(&self.admission)
            .acquire_owned()
            .await
            .map_err(|_| CredentialsError::Store(CredentialStoreFailure::Io))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _invalidate = WriteInvalidation {
                reference: reference.clone(),
                flights,
            };
            store.set(&reference, &secret)
        })
        .await
        .map_err(|_| CredentialsError::OutcomeUnknown)?
    }

    async fn unset(&self, reference: &CredentialRef) -> Result<bool> {
        reference.validate()?;
        let store = Arc::clone(&self.store);
        let flights = Arc::clone(&self.flights);
        let reference = reference.clone();
        let permit = Arc::clone(&self.admission)
            .acquire_owned()
            .await
            .map_err(|_| CredentialsError::Store(CredentialStoreFailure::Io))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _invalidate = WriteInvalidation {
                reference: reference.clone(),
                flights,
            };
            store.unset(&reference)
        })
        .await
        .map_err(|_| CredentialsError::OutcomeUnknown)?
    }
}

/// Ordinary plugin factory for the local credentials provider.
#[derive(Clone, Debug)]
pub struct CredentialsLocalFactory {
    store: Arc<dyn SecretStore>,
    captured_environment: BTreeMap<String, SecretValue>,
}

impl CredentialsLocalFactory {
    /// Creates a provider with an explicit store and captured environment.
    pub fn with_store(
        store: Arc<dyn SecretStore>,
        captured_environment: BTreeMap<String, SecretValue>,
    ) -> Self {
        Self {
            store,
            captured_environment,
        }
    }
}

#[async_trait]
impl PluginFactory for CredentialsLocalFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: CredentialsLocalConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let retained = config
            .environment
            .iter()
            .map(|binding| binding.reference.account().len() + binding.variable.len())
            .sum::<usize>();
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<CredentialsLocalConfig>()?;
        let mut environment = HashMap::new();
        for binding in config.environment {
            if let Some(secret) = self.captured_environment.get(&binding.variable) {
                environment.insert(
                    binding.reference,
                    EnvironmentValue {
                        variable: binding.variable,
                        secret: secret.clone(),
                    },
                );
            }
        }
        let service = Arc::new(Service {
            store: Arc::clone(&self.store),
            environment,
            flights: Arc::new(Mutex::new(HashMap::new())),
            admission: Arc::new(Semaphore::new(config.maximum_concurrent_store_operations)),
            resolution_timeout: Duration::from_millis(config.resolution_timeout_ms),
        });
        let resolve: Arc<dyn CredentialsResolve> = service.clone();
        let admin: Arc<dyn CredentialsAdmin> = service.clone();
        let status: Arc<dyn CredentialsStatus> = service;
        let resolve_supply = plan
            .context()
            .provide_local::<CredentialsResolveContract>(resolve)?;
        let admin_supply = plan
            .context()
            .provide_local::<CredentialsAdminContract>(admin)?;
        let status_supply = plan
            .context()
            .provide_local::<CredentialsStatusContract>(status)?;
        plan.defer(
            "withdraw local credential services",
            Box::new(move || {
                Box::pin(async move {
                    drop(status_supply);
                    drop(admin_supply);
                    drop(resolve_supply);
                    Ok(())
                })
            }),
        )
    }
}

/// Memory store used to inject deterministic local-provider tests.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<CredentialRef, SecretValue>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, reference: &CredentialRef) -> Result<Option<SecretValue>> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(reference)
            .cloned())
    }

    fn set(&self, reference: &CredentialRef, secret: &SecretValue) -> Result<()> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(reference.clone(), secret.clone());
        Ok(())
    }

    fn unset(&self, reference: &CredentialRef) -> Result<bool> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(reference)
            .is_some())
    }
}

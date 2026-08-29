//! OS-keyring and captured-environment credential provider.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_credentials_protocol::{
    CredentialRef, CredentialSource, CredentialsAdmin, CredentialsAdminContract, CredentialsError,
    CredentialsResolve, CredentialsResolveContract, ResolvedCredential, Result, SecretValue,
    validate_environment_name, validate_segment,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Semaphore, watch};

const DEFAULT_MAXIMUM_CONCURRENT_RESOLUTIONS: usize = 8;
const MAXIMUM_CONCURRENT_RESOLUTIONS: usize = 64;
const DEFAULT_RESOLUTION_TIMEOUT_MS: u64 = 30_000;
const MAXIMUM_RESOLUTION_TIMEOUT_MS: u64 = 5 * 60 * 1_000;

/// Minimal secret-store seam used by the local provider.
pub trait SecretStore: fmt::Debug + Send + Sync + 'static {
    /// Reads one exact service/account entry.
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretValue>>;
    /// Sets one exact service/account entry.
    fn set(&self, service: &str, account: &str, secret: &SecretValue) -> Result<()>;
    /// Deletes one exact entry and reports whether it existed.
    fn unset(&self, service: &str, account: &str) -> Result<bool>;
}

/// Platform keyring-backed secret store.
#[derive(Clone, Debug, Default)]
pub struct KeyringSecretStore;

impl SecretStore for KeyringSecretStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretValue>> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|error| CredentialsError::Store(error.to_string()))?;
        match entry.get_password() {
            Ok(secret) => SecretValue::new(secret).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(CredentialsError::Store(error.to_string())),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &SecretValue) -> Result<()> {
        keyring::Entry::new(service, account)
            .and_then(|entry| entry.set_password(secret.expose_secret()))
            .map_err(|error| CredentialsError::Store(error.to_string()))
    }

    fn unset(&self, service: &str, account: &str) -> Result<bool> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|error| CredentialsError::Store(error.to_string()))?;
        match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(CredentialsError::Store(error.to_string())),
        }
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
    /// Stable OS keyring service name.
    pub service: String,
    /// Explicit environment fallback mapping.
    #[serde(default)]
    pub environment: Vec<EnvironmentBinding>,
    /// Maximum synchronous secret-store lookups admitted concurrently.
    #[serde(default = "default_maximum_concurrent_resolutions")]
    pub maximum_concurrent_resolutions: usize,
    /// Per-waiter credential resolution deadline in milliseconds.
    #[serde(default = "default_resolution_timeout_ms")]
    pub resolution_timeout_ms: u64,
}

const fn default_maximum_concurrent_resolutions() -> usize {
    DEFAULT_MAXIMUM_CONCURRENT_RESOLUTIONS
}

const fn default_resolution_timeout_ms() -> u64 {
    DEFAULT_RESOLUTION_TIMEOUT_MS
}

impl CredentialsLocalConfig {
    fn validate(&self) -> Result<()> {
        validate_segment("keyring service", &self.service)?;
        if self.maximum_concurrent_resolutions == 0
            || self.maximum_concurrent_resolutions > MAXIMUM_CONCURRENT_RESOLUTIONS
        {
            return Err(CredentialsError::InvalidInput(format!(
                "maximum_concurrent_resolutions must be within 1..={MAXIMUM_CONCURRENT_RESOLUTIONS}"
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
    name: String,
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
            .map_err(|error| {
                CredentialsError::Store(format!(
                    "credential admission unexpectedly closed: {error}"
                ))
            })?;
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
                let name = self.name.clone();
                let reference = reference.clone();
                let environment = self.environment.get(&reference).cloned();
                let flights = Arc::clone(&self.flights);
                tokio::spawn(async move {
                    let _permit = permit;
                    let account = reference.account();
                    let stored = tokio::task::spawn_blocking(move || store.get(&name, &account))
                        .await
                        .map_err(|error| {
                            CredentialsError::Store(format!("keyring task failed: {error}"))
                        })
                        .and_then(std::convert::identity);
                    let result = resolve_stored(&reference, environment, stored);
                    let _ignored = sender.send(Some(result));
                    flights
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&reference);
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
            return Err(CredentialsError::Store(
                "credential resolution ended without a result".into(),
            ));
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
                source: CredentialSource::Keyring,
            });
        }
        Ok(None) | Err(_) if environment.is_some() => {}
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

#[async_trait]
impl CredentialsAdmin for Service {
    async fn set(&self, reference: &CredentialRef, secret: SecretValue) -> Result<()> {
        reference.validate()?;
        if let Some(value) = self.environment.get(reference) {
            return Err(CredentialsError::EnvironmentShadow(value.variable.clone()));
        }
        let store = Arc::clone(&self.store);
        let name = self.name.clone();
        let account = reference.account();
        tokio::task::spawn_blocking(move || store.set(&name, &account, &secret))
            .await
            .map_err(|error| CredentialsError::Store(format!("keyring task failed: {error}")))?
    }

    async fn unset(&self, reference: &CredentialRef) -> Result<bool> {
        reference.validate()?;
        if let Some(value) = self.environment.get(reference) {
            return Err(CredentialsError::EnvironmentShadow(value.variable.clone()));
        }
        let store = Arc::clone(&self.store);
        let name = self.name.clone();
        let account = reference.account();
        tokio::task::spawn_blocking(move || store.unset(&name, &account))
            .await
            .map_err(|error| CredentialsError::Store(format!("keyring task failed: {error}")))?
    }
}

/// Ordinary plugin factory for the local credentials provider.
#[derive(Clone, Debug)]
pub struct CredentialsLocalFactory {
    store: Arc<dyn SecretStore>,
    captured_environment: BTreeMap<String, SecretValue>,
}

impl Default for CredentialsLocalFactory {
    fn default() -> Self {
        Self {
            store: Arc::new(KeyringSecretStore),
            captured_environment: BTreeMap::new(),
        }
    }
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
        let retained = config.service.len()
            + config
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
            name: config.service,
            store: Arc::clone(&self.store),
            environment,
            flights: Arc::new(Mutex::new(HashMap::new())),
            admission: Arc::new(Semaphore::new(config.maximum_concurrent_resolutions)),
            resolution_timeout: Duration::from_millis(config.resolution_timeout_ms),
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
            "withdraw local credential services",
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

/// Memory store used to inject deterministic local-provider tests.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<(String, String), SecretValue>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretValue>> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, secret: &SecretValue) -> Result<()> {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((service.to_owned(), account.to_owned()), secret.clone());
        Ok(())
    }

    fn unset(&self, service: &str, account: &str) -> Result<bool> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(service.to_owned(), account.to_owned()))
            .is_some())
    }
}

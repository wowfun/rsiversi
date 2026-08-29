//! Active Settings namespace registry plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{
    Result, Settings, SettingsContract, SettingsDocument, SettingsError, SettingsLease,
    SettingsProvider, SettingsProviderContract, SettingsRegistration, SettingsScope,
    SettingsSnapshot, SettingsSpec, validate_namespace, validate_section,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Debug)]
struct Service {
    provider: Arc<dyn SettingsProvider>,
    state: Arc<ServiceState>,
}

#[derive(Debug)]
struct ServiceState {
    inner: Mutex<ServiceInner>,
    write_lock: Arc<AsyncMutex<()>>,
}

#[derive(Debug)]
struct ServiceInner {
    raw: SettingsDocument,
    next_registration: u64,
    namespaces: HashMap<String, NamespaceState>,
}

#[derive(Debug)]
struct NamespaceState {
    registration: u64,
    revision: u64,
    raw: Option<Value>,
    resolved: Value,
    defaults: Value,
    base: Value,
    validator: Arc<dyn rsi_settings_protocol::SettingsValidator>,
    in_flight: usize,
    retiring: bool,
}

#[derive(Debug)]
struct Scope {
    namespace: String,
    registration: u64,
    provider: Arc<dyn SettingsProvider>,
    state: Weak<ServiceState>,
}

struct InFlightCommit {
    state: Arc<ServiceState>,
    namespace: String,
    registration: u64,
}

impl Drop for InFlightCommit {
    fn drop(&mut self) {
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remove = inner
            .namespaces
            .get_mut(&self.namespace)
            .filter(|entry| entry.registration == self.registration)
            .is_some_and(|entry| {
                entry.in_flight = entry.in_flight.saturating_sub(1);
                entry.retiring && entry.in_flight == 0
            });
        if remove {
            inner.namespaces.remove(&self.namespace);
        }
    }
}

impl Settings for Service {
    fn register(&self, spec: SettingsSpec) -> Result<SettingsRegistration> {
        validate_namespace(&spec.namespace)?;
        validate_section(&spec.defaults)?;
        validate_section(&spec.base)?;
        let mut state = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.namespaces.contains_key(&spec.namespace) {
            return Err(SettingsError::DuplicateNamespace(spec.namespace));
        }
        let raw = state.raw.get(&spec.namespace).cloned();
        if let Some(raw) = &raw {
            validate_section(raw)?;
        }
        let resolved = resolve(&spec.defaults, &spec.base, raw.as_ref());
        validate_section(&resolved)?;
        spec.validator.validate(&resolved)?;
        state.next_registration = state
            .next_registration
            .checked_add(1)
            .ok_or_else(|| SettingsError::InvalidInput("registration identity exhausted".into()))?;
        let registration = state.next_registration;
        state.namespaces.insert(
            spec.namespace.clone(),
            NamespaceState {
                registration,
                revision: 0,
                raw,
                resolved,
                defaults: spec.defaults,
                base: spec.base,
                validator: spec.validator,
                in_flight: 0,
                retiring: false,
            },
        );
        let namespace = spec.namespace;
        let weak = Arc::downgrade(&self.state);
        let scope: Arc<dyn SettingsScope> = Arc::new(Scope {
            namespace: namespace.clone(),
            registration,
            provider: Arc::clone(&self.provider),
            state: weak.clone(),
        });
        let lease = SettingsLease::new(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let mut inner = state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(entry) = inner
                .namespaces
                .get_mut(&namespace)
                .filter(|entry| entry.registration == registration)
            {
                if entry.in_flight == 0 {
                    inner.namespaces.remove(&namespace);
                } else {
                    entry.retiring = true;
                }
            }
        });
        Ok(SettingsRegistration { scope, lease })
    }
}

#[async_trait]
impl SettingsScope for Scope {
    fn get(&self) -> Result<SettingsSnapshot> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| SettingsError::StaleRegistration(self.namespace.clone()))?;
        let inner = state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = active_entry(&inner, &self.namespace, self.registration)?;
        Ok(SettingsSnapshot {
            revision: entry.revision,
            value: entry.resolved.clone(),
        })
    }

    async fn replace(&self, expected_revision: u64, value: Value) -> Result<SettingsSnapshot> {
        validate_section(&value)?;
        self.update(expected_revision, Some(value)).await
    }

    async fn clear(&self, expected_revision: u64) -> Result<SettingsSnapshot> {
        self.update(expected_revision, None).await
    }
}

impl Scope {
    async fn update(
        &self,
        expected_revision: u64,
        replacement: Option<Value>,
    ) -> Result<SettingsSnapshot> {
        if !self.provider.writable() {
            return Err(SettingsError::ReadOnly);
        }
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| SettingsError::StaleRegistration(self.namespace.clone()))?;
        let write_guard = Arc::clone(&state.write_lock).lock_owned().await;
        let (raw, defaults, base, validator) = {
            let inner = state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = active_entry(&inner, &self.namespace, self.registration)?;
            if entry.revision != expected_revision {
                return Err(SettingsError::Conflict {
                    expected: expected_revision,
                    actual: entry.revision,
                });
            }
            (
                entry.raw.clone(),
                entry.defaults.clone(),
                entry.base.clone(),
                Arc::clone(&entry.validator),
            )
        };
        let resolved = resolve(&defaults, &base, replacement.as_ref());
        validate_section(&resolved)?;
        validator.validate(&resolved)?;
        {
            let mut inner = state
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = active_entry_mut(&mut inner, &self.namespace, self.registration)?;
            if entry.revision != expected_revision {
                return Err(SettingsError::Conflict {
                    expected: expected_revision,
                    actual: entry.revision,
                });
            }
            entry.in_flight = entry.in_flight.checked_add(1).ok_or_else(|| {
                SettingsError::InvalidInput("settings operation count exhausted".into())
            })?;
        }
        let provider = Arc::clone(&self.provider);
        let namespace = self.namespace.clone();
        let registration = self.registration;
        tokio::spawn(async move {
            let in_flight = InFlightCommit {
                state: Arc::clone(&state),
                namespace: namespace.clone(),
                registration,
            };
            let committed = provider
                .compare_and_set(&namespace, raw.as_ref(), replacement.as_ref())
                .await;
            let result = publish_settings_commit(
                &state,
                &namespace,
                registration,
                expected_revision,
                committed,
            );
            drop(in_flight);
            drop(write_guard);
            result
        })
        .await
        .map_err(|error| SettingsError::Io(format!("Settings commit task failed: {error}")))?
    }
}

fn active_entry<'a>(
    inner: &'a ServiceInner,
    namespace: &str,
    registration: u64,
) -> Result<&'a NamespaceState> {
    inner
        .namespaces
        .get(namespace)
        .filter(|entry| entry.registration == registration && !entry.retiring)
        .ok_or_else(|| SettingsError::StaleRegistration(namespace.to_owned()))
}

fn active_entry_mut<'a>(
    inner: &'a mut ServiceInner,
    namespace: &str,
    registration: u64,
) -> Result<&'a mut NamespaceState> {
    inner
        .namespaces
        .get_mut(namespace)
        .filter(|entry| entry.registration == registration && !entry.retiring)
        .ok_or_else(|| SettingsError::StaleRegistration(namespace.to_owned()))
}

fn publish_settings_commit(
    state: &ServiceState,
    namespace: &str,
    registration: u64,
    expected_revision: u64,
    committed: Result<Option<Value>>,
) -> Result<SettingsSnapshot> {
    let mut inner = state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Ok(committed) = &committed {
        match committed {
            Some(value) => {
                inner.raw.insert(namespace.to_owned(), value.clone());
            }
            None => {
                inner.raw.remove(namespace);
            }
        }
    }

    match inner
        .namespaces
        .get_mut(namespace)
        .filter(|entry| entry.registration == registration)
    {
        Some(entry) => match committed {
            Ok(committed) => {
                if entry.revision == expected_revision {
                    entry.revision = entry.revision.checked_add(1).ok_or_else(|| {
                        SettingsError::InvalidInput("settings revision exhausted".into())
                    })?;
                    entry.raw.clone_from(&committed);
                    entry.resolved = resolve(&entry.defaults, &entry.base, committed.as_ref());
                    Ok(SettingsSnapshot {
                        revision: entry.revision,
                        value: entry.resolved.clone(),
                    })
                } else {
                    Err(SettingsError::Conflict {
                        expected: expected_revision,
                        actual: entry.revision,
                    })
                }
            }
            Err(error) => Err(error),
        },
        None => Err(SettingsError::StaleRegistration(namespace.to_owned())),
    }
}

fn resolve(defaults: &Value, base: &Value, user: Option<&Value>) -> Value {
    let mut value = defaults.clone();
    merge(&mut value, base);
    if let Some(user) = user {
        merge(&mut value, user);
    }
    value
}

fn merge(target: &mut Value, overlay: &Value) {
    match (target, overlay) {
        (Value::Object(target), Value::Object(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = target.get_mut(key) {
                    merge(existing, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, overlay) => {
            *target = overlay.clone();
        }
    }
}

/// Ordinary plugin factory for one Settings registry generation.
#[derive(Clone, Debug, Default)]
pub struct SettingsFactory;

#[async_trait]
impl PluginFactory for SettingsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Settings configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null).requiring_local::<SettingsProviderContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let provider = plan.local::<SettingsProviderContract>()?;
        let raw = provider
            .load()
            .await
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        for (namespace, section) in &raw {
            validate_namespace(namespace)
                .and_then(|()| validate_section(section).map(|_| ()))
                .map_err(|error| MetaError::Activation(error.to_string()))?;
        }
        let settings: Arc<dyn Settings> = Arc::new(Service {
            provider,
            state: Arc::new(ServiceState {
                inner: Mutex::new(ServiceInner {
                    raw,
                    next_registration: 0,
                    namespaces: HashMap::new(),
                }),
                write_lock: Arc::new(AsyncMutex::new(())),
            }),
        });
        let supply = plan.context().provide_local::<SettingsContract>(settings)?;
        plan.defer(
            "withdraw Settings registry",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

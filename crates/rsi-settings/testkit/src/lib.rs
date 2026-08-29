//! Deterministic Settings provider for keyless tests.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{
    Result, SettingsDocument, SettingsError, SettingsProvider, SettingsProviderContract,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct MemoryProvider {
    document: Mutex<SettingsDocument>,
}

#[async_trait]
impl SettingsProvider for MemoryProvider {
    fn writable(&self) -> bool {
        true
    }

    async fn load(&self) -> Result<SettingsDocument> {
        Ok(self
            .document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    async fn compare_and_set(
        &self,
        namespace: &str,
        expected: Option<&Value>,
        replacement: Option<&Value>,
    ) -> Result<Option<Value>> {
        let mut document = self
            .document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if document.get(namespace) != expected {
            return Err(SettingsError::ConcurrentDocumentChange);
        }
        match replacement {
            Some(value) => {
                document.insert(namespace.to_owned(), value.clone());
            }
            None => {
                document.remove(namespace);
            }
        }
        Ok(document.get(namespace).cloned())
    }
}

/// Factory for a deterministic memory Settings provider.
#[derive(Clone, Debug)]
pub struct MemorySettingsProviderFactory {
    initial: SettingsDocument,
}

impl MemorySettingsProviderFactory {
    /// Creates a provider from a JSON object indexed by namespace.
    pub fn new(initial: Value) -> Self {
        let initial = match initial {
            Value::Object(object) => object.into_iter().collect(),
            _ => SettingsDocument::new(),
        };
        Self { initial }
    }
}

#[async_trait]
impl PluginFactory for MemorySettingsProviderFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "memory Settings provider configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null))
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let provider: Arc<dyn SettingsProvider> = Arc::new(MemoryProvider {
            document: Mutex::new(self.initial.clone()),
        });
        let supply = plan
            .context()
            .provide_local::<SettingsProviderContract>(provider)?;
        plan.defer(
            "withdraw memory Settings provider",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

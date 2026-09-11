use super::{SettingsDefaultStore, plugin::settings_spec};
use async_trait::async_trait;
use rsi_agent_presets::{AgentPresetDefaultStore, AgentPresetId, PresetError};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::SettingsContract;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
struct Selection {
    path: PathBuf,
    current: Mutex<Option<SettingsDefaultStore>>,
}
impl Selection {
    fn store(&self) -> rsi_agent_presets::Result<SettingsDefaultStore> {
        self.current
            .lock()
            .expect("preset selection poisoned")
            .clone()
            .ok_or_else(|| PresetError::Io {
                operation: "read",
                path: self.path.clone(),
                message: "Service preset settings are unavailable".into(),
            })
    }
}
#[async_trait]
impl AgentPresetDefaultStore for Selection {
    async fn load(&self) -> rsi_agent_presets::Result<Option<AgentPresetId>> {
        self.store()?.load().await
    }
    async fn replace(&self, selected: Option<AgentPresetId>) -> rsi_agent_presets::Result<()> {
        self.store()?.replace(selected).await
    }
}
#[derive(Debug)]
pub(crate) struct ServicePresetSettingsFactory {
    selection: Arc<Selection>,
}
impl ServicePresetSettingsFactory {
    pub(crate) fn new(path: PathBuf) -> (Self, Arc<dyn AgentPresetDefaultStore>) {
        let selection = Arc::new(Selection {
            path,
            current: Mutex::new(None),
        });
        (
            Self {
                selection: selection.clone(),
            },
            selection,
        )
    }
}
#[async_trait]
impl PluginFactory for ServicePresetSettingsFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "preset settings configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(config.clone()).requiring_local::<SettingsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registration = plan
            .local::<SettingsContract>()?
            .register(settings_spec())
            .map_err(|error| MetaError::Activation(error.to_string()))?;
        let scope = registration.scope.clone();
        *self
            .selection
            .current
            .lock()
            .expect("preset selection poisoned") = Some(SettingsDefaultStore {
            scope: scope.clone(),
            path: self.selection.path.clone(),
        });
        let selection = self.selection.clone();
        plan.defer(
            "withdraw Service preset selection",
            Box::new(move || {
                Box::pin(async move {
                    let mut current = selection.current.lock().expect("preset selection poisoned");
                    if current
                        .as_ref()
                        .is_some_and(|store| Arc::ptr_eq(&store.scope, &scope))
                    {
                        *current = None;
                    }
                    drop(registration);
                    Ok(())
                })
            }),
        )
    }
}

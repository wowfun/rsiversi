use super::{AgentPresetDefaults, AgentPresetDefaultsContract, plugin::settings_spec};
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
    current: Mutex<Option<Arc<AgentPresetDefaults>>>,
}
impl Selection {
    fn store(&self) -> rsi_agent_presets::Result<Arc<AgentPresetDefaults>> {
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
        let store = Arc::new(AgentPresetDefaults {
            scope: scope.clone(),
            path: self.selection.path.clone(),
        });
        plan.context()
            .provide_local::<AgentPresetDefaultsContract>(store.clone())?;
        *self
            .selection
            .current
            .lock()
            .expect("preset selection poisoned") = Some(store);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
    use rsi_settings_protocol::{SettingsContract, SettingsError};
    use serde_json::json;

    #[tokio::test]
    async fn default_choice_preserves_user_layers_and_rejects_stale_edits() {
        let runtime = Runtime::default();
        let (owner, catalog) = ServicePresetSettingsFactory::new(PathBuf::from("fixture"));
        let plugins: [(&str, Arc<dyn PluginFactory>); 3] = [
            (
                "provider",
                Arc::new(rsi_settings_testkit::MemorySettingsProviderFactory::new(
                    json!({}),
                )),
            ),
            ("settings", Arc::new(rsi_settings::SettingsFactory)),
            ("presets", Arc::new(owner)),
        ];
        for (id, plugin) in plugins {
            runtime
                .root()
                .apply(
                    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, plugin),
                    ConfigValue::Null,
                )
                .await
                .unwrap();
        }
        let defaults = runtime
            .root()
            .lookup_local::<AgentPresetDefaultsContract>()
            .unwrap();
        let scope = runtime
            .root()
            .lookup_local::<SettingsContract>()
            .unwrap()
            .scope(crate::AGENT_PRESET_SETTINGS_NAMESPACE)
            .unwrap();
        let (initial, choice) = defaults.choice().await.unwrap();
        assert_eq!(choice, None);
        let standard = AgentPresetId::new("standard").unwrap();
        let explicit = defaults
            .set_choice(Some(&initial), Some(&standard))
            .await
            .unwrap();
        assert_eq!(
            defaults.choice().await.unwrap(),
            (explicit.clone(), Some(standard.clone()))
        );
        assert_eq!(
            scope.user_value(None).await.unwrap().1,
            Some(json!({"default":"standard"}))
        );
        assert_eq!(
            defaults
                .set_choice(Some(&explicit), Some(&standard))
                .await
                .unwrap(),
            explicit
        );
        assert!(matches!(
            defaults.set_choice(Some(&initial), None).await,
            Err(SettingsError::Conflict { .. })
        ));
        defaults.set_choice(Some(&explicit), None).await.unwrap();
        assert_eq!(scope.user_value(None).await.unwrap().1, None);

        let root = tempfile::tempdir().unwrap();
        let roots = json!({"roots":[{"path":root.path()}]});
        scope
            .replace(scope.get().unwrap().revision, roots.clone())
            .await
            .unwrap();
        let app = AgentPresetId::new("minimal").unwrap();
        defaults.set_choice(None, Some(&app)).await.unwrap();
        assert_eq!(catalog.load().await.unwrap(), Some(app));
        catalog.replace(None).await.unwrap();
        assert_eq!(scope.user_value(None).await.unwrap().1, Some(roots));
        assert!(runtime.shutdown().await.is_clean());
        assert!(matches!(
            defaults.choice().await,
            Err(SettingsError::StaleRegistration(_))
        ));
    }
}

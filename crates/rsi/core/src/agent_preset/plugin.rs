use super::{
    AGENT_PRESET_SETTINGS_NAMESPACE, DEFAULT_AGENT_PRESET_ID, SettingsDefaultStore,
    SystemPresetSource, read_settings, settings_boot, user_agent_preset_root, validate_settings,
};
use async_trait::async_trait;
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetDefaultStore, AgentPresetId,
    AgentPresetProfileCompiler, AgentPresetRoot,
};
use rsi_host::HostPaths;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_settings_protocol::{SettingsContract, SettingsSpec, ValidateWith};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct CatalogContract;
impl LocalContract for CatalogContract {
    const KEY: &'static str = "rsi.agent.preset-catalog";
    type Service = AgentPresetCatalog;
}

#[derive(Debug)]
pub(super) struct CatalogFactory {
    pub paths: HostPaths,
    pub sources: Vec<SystemPresetSource>,
    pub compiler: AgentPresetProfileCompiler,
    pub diagnostic: std::sync::Mutex<Option<String>>,
}
impl CatalogFactory {
    fn diagnosed(&self, error: impl std::fmt::Display) -> MetaError {
        let mut message = error.to_string();
        let mut end = message.len().min(4096);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        *self.diagnostic.lock().expect("catalog diagnostic poisoned") = Some(message.clone());
        MetaError::Activation(message)
    }
}
#[async_trait]
impl PluginFactory for CatalogFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "Agent-preset catalog configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<SettingsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let settings = plan.local::<SettingsContract>()?;
        let registration = settings
            .register(settings_spec())
            .map_err(|error| self.diagnosed(settings_boot(error)))?;
        let scope = registration.scope.clone();
        plan.defer(
            "withdraw Agent-preset Settings namespace",
            Box::new(move || {
                Box::pin(async move {
                    drop(registration);
                    Ok(())
                })
            }),
        )?;
        let wire =
            read_settings(scope.as_ref()).map_err(|error| self.diagnosed(settings_boot(error)))?;
        let default = AgentPresetId::new(DEFAULT_AGENT_PRESET_ID).map_err(activation)?;
        let mut config = AgentPresetCatalogConfig::new(default);
        for source in &self.sources {
            config = match source {
                SystemPresetSource::Root(path) => config.with_system_root(path.clone()),
                SystemPresetSource::Exact { id, path } => {
                    config.with_system_preset(id.clone(), path.clone())
                }
            };
        }
        for root in wire.roots {
            config = config.with_configured_root(
                AgentPresetRoot::new(root.path, root.trust.into())
                    .map_err(|error| self.diagnosed(error))?,
            );
        }
        config = config.with_user_root(user_agent_preset_root(&self.paths));
        let defaults: Arc<dyn AgentPresetDefaultStore> = Arc::new(SettingsDefaultStore {
            scope,
            path: self.paths.config().join("settings.json"),
        });
        let catalog =
            AgentPresetCatalog::with_default_store(config, defaults, self.compiler.clone())
                .map_err(|error| self.diagnosed(error))?;
        let supply = plan
            .context()
            .provide_local::<CatalogContract>(Arc::new(catalog))?;
        plan.defer(
            "withdraw Agent-preset catalog",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

pub(super) fn settings_spec() -> SettingsSpec {
    SettingsSpec {
                namespace: AGENT_PRESET_SETTINGS_NAMESPACE.into(),
                defaults: json!({"default":DEFAULT_AGENT_PRESET_ID,"roots":[]}),
                base: json!({}),
                metadata: rsi_settings_protocol::SettingsMetadata {
                    schema: json!({"type":"object","additionalProperties":false,"required":["default","roots"],"properties":{
                        "default":{"type":"string","description":"Default selection for future drafts."},
                        "roots":{"type":"array","description":"Absolute discovery roots; changing roots requires restarting the catalog.","items":{"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"trust":{"enum":["system","user"]}}}}
                    }}),
                    applies: rsi_settings_protocol::SettingsApply::Restart,
                    description: "Root changes require restarting the catalog. The default preset is read for future selections without restarting; existing Sessions retain their pinned composition.".into(),
                    sensitive_fields: vec![vec!["roots".into()]],
                },
                validator: Arc::new(ValidateWith(validate_settings)),
            }
}

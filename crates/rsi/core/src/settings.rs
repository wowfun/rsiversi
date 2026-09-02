use async_trait::async_trait;
use rsi_agent_session_protocol::FrozenAgentSettings;
use rsi_meta::{ActivationPlan, ConfigValue, LocalContract, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{SettingsContract, SettingsError, SettingsSpec, ValidateWith};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) const SETTINGS_FACTORY: &str = "rsi.session.settings";
const SETTINGS_NAMESPACE: &str = "rsi.agent";

/// Frozen standard Agent defaults resolved from Settings during boot.
pub trait AgentSettings: std::fmt::Debug + Send + Sync + 'static {
    /// Returns the validated creation-time Agent settings template.
    fn current(&self) -> &FrozenAgentSettings;
}

/// Nominal Local contract for standard Agent defaults.
#[derive(Debug)]
pub struct AgentSettingsContract;

impl LocalContract for AgentSettingsContract {
    const KEY: &'static str = "rsi.session.agent_settings";
    type Service = dyn AgentSettings;
}

#[derive(Debug)]
struct Service {
    settings: FrozenAgentSettings,
}

impl AgentSettings for Service {
    fn current(&self) -> &FrozenAgentSettings {
        &self.settings
    }
}

/// Ordinary Settings consumer for the standard Session Host.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentSettingsFactory;

#[async_trait]
impl PluginFactory for AgentSettingsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(rsi_meta::MetaError::InvalidInput(
                "Session Agent Settings configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null).requiring_local::<SettingsContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let settings = plan.local::<SettingsContract>()?;
        let registration = settings
            .register(SettingsSpec {
                namespace: SETTINGS_NAMESPACE.into(),
                defaults: json!({
                    "settings_id": "standard",
                    "system_prompt": "You are a careful coding agent.",
                    "sandbox": "workspace-write",
                    "require_approval": false,
                    "turn_budget": {
                        "maximum_elapsed_ms": 1_800_000,
                        "maximum_provider_attempts": 64,
                        "maximum_tool_calls": 256,
                        "maximum_generated_facts": 65_536,
                        "maximum_generated_fact_bytes": 67_108_864
                    }
                }),
                base: json!({}),
                validator: Arc::new(ValidateWith(validate_settings)),
            })
            .map_err(|error| settings_meta(&error))?;
        let snapshot = registration
            .scope
            .get()
            .map_err(|error| settings_meta(&error))?;
        let settings: FrozenAgentSettings =
            serde_json::from_value(snapshot.value).map_err(|error| {
                rsi_meta::MetaError::Activation(format!(
                    "invalid `{SETTINGS_NAMESPACE}` Settings: {error}"
                ))
            })?;
        let service: Arc<dyn AgentSettings> = Arc::new(Service { settings });
        let supply = plan
            .context()
            .provide_local::<AgentSettingsContract>(service)?;
        plan.defer(
            "withdraw Session Agent Settings",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    drop(registration);
                    Ok(())
                })
            }),
        )
    }
}

fn validate_settings(value: &Value) -> rsi_settings_protocol::Result<()> {
    if value.get("default_model").is_none() {
        return Err(SettingsError::InvalidInput(
            "`rsi.agent.default_model` is required; configure its `deployment` and `model` fields"
                .into(),
        ));
    }
    serde_json::from_value::<FrozenAgentSettings>(value.clone())
        .map(|_| ())
        .map_err(|error| SettingsError::InvalidInput(error.to_string()))
}

fn settings_meta(error: &SettingsError) -> rsi_meta::MetaError {
    rsi_meta::MetaError::Activation(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_settings;
    use serde_json::json;

    #[test]
    fn missing_explicit_default_model_has_an_actionable_setting_path() {
        let error = validate_settings(&json!({
            "settings_id": "standard",
            "system_prompt": "system",
            "sandbox": "workspace-write",
            "require_approval": false
        }))
        .expect_err("the standard product has no implicit provider deployment");
        assert!(error.to_string().contains("rsi.agent.default_model"));
        assert!(error.to_string().contains("deployment"));
        assert!(error.to_string().contains("model"));
    }
}

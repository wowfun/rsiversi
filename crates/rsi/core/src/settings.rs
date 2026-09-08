use async_trait::async_trait;
use rsi_agent_session_protocol::FrozenAgentSettings;
use rsi_meta::{ActivationPlan, ConfigValue, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{SettingsContract, SettingsError, SettingsSpec, ValidateWith};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) const SETTINGS_FACTORY: &str = "rsi.agent.defaults";
const SETTINGS_NAMESPACE: &str = "rsi.agent";

use rsi_session_protocol::{AgentSettingsContract, AgentSettingsSource, SessionError};

#[derive(Debug)]
struct Service {
    scope: Arc<dyn rsi_settings_protocol::SettingsScope>,
}

impl AgentSettingsSource for Service {
    fn current(&self) -> rsi_session_protocol::Result<FrozenAgentSettings> {
        let snapshot = self
            .scope
            .get()
            .map_err(|error| SessionError::Backend(error.to_string()))?;
        serde_json::from_value(snapshot.value)
            .map_err(|error| SessionError::Backend(error.to_string()))
    }
}

/// Ordinary Settings consumer for the standard Service Host.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentSettingsFactory;

#[async_trait]
impl PluginFactory for AgentSettingsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(rsi_meta::MetaError::InvalidInput(
                "Agent defaults configuration must be null or empty".into(),
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
        let service = Service {
            scope: registration.scope.clone(),
        };
        service
            .current()
            .map_err(|error| rsi_meta::MetaError::Activation(error.to_string()))?;
        let service: Arc<dyn AgentSettingsSource> = Arc::new(service);
        let supply = plan
            .context()
            .provide_local::<AgentSettingsContract>(service)?;
        plan.defer(
            "withdraw Agent defaults",
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

    #[tokio::test]
    async fn defaults_read_current_validated_settings_and_retire_with_their_scope() {
        use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
        use rsi_session_protocol::AgentSettingsContract;
        use rsi_settings_protocol::SettingsContract;
        use std::sync::Arc;

        let runtime = Runtime::default();
        let provider = runtime.root().apply(ResolvedFactory::linked("settings-memory", "test", UpdateMode::Replayable,
            Arc::new(rsi_settings_testkit::MemorySettingsProviderFactory::new(json!({
                "rsi.agent": { "default_model": {"deployment": "fixture", "model": "old"} }
            })))), serde_json::Value::Null).await.unwrap();
        let settings_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "settings",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(rsi_settings::SettingsFactory),
                ),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        let defaults_fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "defaults",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(super::AgentSettingsFactory),
                ),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        let defaults = runtime
            .root()
            .lookup_local::<AgentSettingsContract>()
            .unwrap();
        let old = defaults.current().unwrap();
        let settings = runtime.root().lookup_local::<SettingsContract>().unwrap();
        let scope = settings.scope("rsi.agent").unwrap();
        scope
            .replace(
                0,
                json!({"default_model": {"deployment": "fixture", "model": "new"}}),
            )
            .await
            .unwrap();
        assert_ne!(defaults.current().unwrap(), old);
        assert_eq!(old.default_model().model(), "old");
        assert!(
            scope
                .replace(1, json!({"default_model": false}))
                .await
                .is_err()
        );
        assert_eq!(defaults.current().unwrap().default_model().model(), "new");
        assert!(defaults_fiber.dispose().await.is_clean());
        assert!(defaults.current().is_err());
        assert!(settings_fiber.dispose().await.is_clean());
        assert!(provider.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_clean());
    }

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

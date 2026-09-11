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
        if snapshot.value.get("default_model").is_none() {
            return Err(SessionError::Backend("Setup required: configure `rsi.agent.default_model` with its `deployment` and `model` fields".into()));
        }
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
                        "maximum_generated_records": 65_536,
                        "maximum_generated_record_bytes": 67_108_864
                    }
                }),
                base: json!({}),
                metadata: metadata(),
                validator: Arc::new(ValidateWith(validate_settings)),
            })
            .map_err(|error| settings_meta(&error))?;
        let service = Service {
            scope: registration.scope.clone(),
        };
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

fn metadata() -> rsi_settings_protocol::SettingsMetadata {
    rsi_settings_protocol::SettingsMetadata {
        schema: json!({
            "type":"object", "additionalProperties":false,
            "required":["settings_id","system_prompt","sandbox","require_approval","turn_budget"],
            "properties": {
                "settings_id":{"type":"string","description":"Immutable settings identity captured in each Session."},
                "system_prompt":{"type":"string"},
                "default_model":{"type":"object","additionalProperties":false,"required":["deployment","model"],"properties":{"deployment":{"type":"string"},"model":{"type":"string"}}},
                "sandbox":{"enum":["read-only","workspace-write","danger-full-access"]},
                "require_approval":{"type":"boolean"},
                "turn_budget":{"type":"object","additionalProperties":false,"properties":{
                    "maximum_elapsed_ms":{"type":"integer","minimum":0},
                    "maximum_provider_attempts":{"type":"integer","minimum":0},
                    "maximum_tool_calls":{"type":"integer","minimum":0},
                    "maximum_generated_records":{"type":"integer","minimum":0},
                    "maximum_generated_record_bytes":{"type":"integer","minimum":0}
                },"description":"The Agent validator enforces its exact bounded Turn budget."}
            }
        }),
        applies: rsi_settings_protocol::SettingsApply::NewSession,
        description: "New conversations capture these values. Existing drafts and durable Sessions keep their original defaults; explicit Turn inputs retain their existing override rules.".into(),
        sensitive_fields: vec![],
    }
}

fn validate_settings(value: &Value) -> rsi_settings_protocol::Result<()> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Configuration {
        settings_id: String,
        system_prompt: String,
        #[serde(default)]
        default_model: Option<rsi_ai_protocol::ModelRef>,
        sandbox: rsi_sandbox::SandboxMode,
        require_approval: bool,
        turn_budget: rsi_agent_session_protocol::TurnBudget,
    }
    let config: Configuration = serde_json::from_value(value.clone())
        .map_err(|error| SettingsError::InvalidInput(error.to_string()))?;
    if value.get("default_model").is_some() && config.default_model.is_none() {
        return Err(SettingsError::InvalidInput(
            "default_model must be absent or an exact deployment/model object".into(),
        ));
    }
    FrozenAgentSettings::validate_policy(
        &config.settings_id,
        &config.system_prompt,
        config.sandbox,
        config.require_approval,
        &config.turn_budget,
    )
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

    #[tokio::test]
    async fn unconfigured_defaults_stay_active_and_become_ready_without_reactivation() {
        use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
        use rsi_session_protocol::AgentSettingsContract;
        use rsi_settings_protocol::SettingsContract;
        use std::sync::Arc;
        let runtime = Runtime::default();
        let root = runtime.root();
        root.apply(
            ResolvedFactory::linked(
                "settings-memory",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_settings_testkit::MemorySettingsProviderFactory::new(
                    json!({}),
                )),
            ),
            json!(null),
        )
        .await
        .unwrap();
        root.apply(
            ResolvedFactory::linked(
                "settings",
                "test",
                UpdateMode::Replayable,
                Arc::new(rsi_settings::SettingsFactory),
            ),
            json!(null),
        )
        .await
        .unwrap();
        let fiber = root
            .apply(
                ResolvedFactory::linked(
                    "defaults",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(super::AgentSettingsFactory),
                ),
                json!(null),
            )
            .await
            .unwrap();
        assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
        let defaults = root.lookup_local::<AgentSettingsContract>().unwrap();
        let error = defaults.current().unwrap_err().to_string();
        assert!(error.contains("rsi.agent.default_model"));
        let settings = root.lookup_local::<SettingsContract>().unwrap();
        let scope = settings.scope("rsi.agent").unwrap();
        let unconfigured = scope.get().unwrap().value;
        validate_settings(&unconfigured).unwrap();
        for change in [
            json!({"default_model":null}),
            json!({"sandbox":"danger-full-access"}),
            json!({"turn_budget":{"maximum_tool_calls":999_999_999}}),
        ] {
            assert!(scope.replace(0, change).await.is_err());
        }
        scope
            .replace(
                0,
                json!({"default_model":{"deployment":"fixture","model":"selected"}}),
            )
            .await
            .unwrap();
        assert_eq!(
            defaults.current().unwrap().default_model().model(),
            "selected"
        );
        assert_eq!(fiber.snapshot().state, rsi_meta::FiberState::Active);
        assert!(runtime.shutdown().await.is_clean());
    }
}

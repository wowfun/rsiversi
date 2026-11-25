use crate::execution_support::{DriveFailure, failed};
use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
    ResolvedFactory, UpdateMode,
};
use rsi_settings_protocol::{
    SettingsApply, SettingsContract, SettingsError, SettingsMetadata, SettingsRegistration,
    SettingsSpec, ValidateWith,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct Parallelism {
    base: usize,
    settings: Mutex<Option<SettingsRegistration>>,
}

impl Parallelism {
    pub(super) async fn attach(ctx: &Context, base: usize) -> rsi_meta::Result<Arc<Self>> {
        let limits = Arc::new(Self {
            base,
            settings: Mutex::default(),
        });
        ctx.apply(
            ResolvedFactory::linked(
                "rsi.agent.loop-settings",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::Replayable,
                Arc::new(LoopSettings(limits.clone())),
            ),
            Value::Null,
        )
        .await?;
        Ok(limits)
    }

    pub(super) async fn limit(&self) -> Result<usize, DriveFailure> {
        let scope = self
            .settings
            .lock()
            .expect("loop settings poisoned")
            .as_ref()
            .map(|registration| registration.scope.clone());
        let Some(scope) = scope else {
            return Ok(self.base);
        };
        let (_, selected) = scope
            .user_value(Some("max_parallel_tool_calls"))
            .await
            .map_err(|error| failed("loop_settings", error.to_string()))?;
        Ok(selected.map_or(self.base, |value| {
            value.as_f64().expect("validated tool limit") as usize
        }))
    }
}

#[derive(Debug)]
struct LoopSettings(Arc<Parallelism>);

#[async_trait]
impl PluginFactory for LoopSettings {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(Value::Null).requiring_local::<SettingsContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let base = self.0.base;
        // A caller can edit the namespace as soon as register returns.
        let mut active = self.0.settings.lock().expect("loop settings poisoned");
        let registration = plan.local::<SettingsContract>()?.register(SettingsSpec {
            namespace: "rsi.agent-loop".into(),
            defaults: json!({"max_parallel_tool_calls":base}),
            base: json!({}),
            metadata: SettingsMetadata {
                applies: SettingsApply::Live,
                description: "Maximum parallel tool calls in each new batch.".into(),
                schema: json!({"type":"object","additionalProperties":false,"required":["max_parallel_tool_calls"],
                    "properties":{"max_parallel_tool_calls":{"type":"integer","minimum":1,"default":base}}}),
                sensitive_fields: Vec::new(),
            },
            validator: Arc::new(ValidateWith(|value: &Value| {
                if value.as_object().is_some_and(|fields| fields.len() == 1)
                    && value["max_parallel_tool_calls"].as_f64().is_some_and(|n| n >= 1.0 && n.fract() == 0.0) {
                    Ok(())
                } else {
                    Err(SettingsError::InvalidInput("max_parallel_tool_calls must be a positive integer".into()))
                }
            })),
        }).map_err(|error| MetaError::Activation(error.to_string()))?;
        let limits = self.0.clone();
        plan.defer(
            "withdraw executor loop settings",
            Box::new(move || {
                Box::pin(async move {
                    limits
                        .settings
                        .lock()
                        .expect("loop settings poisoned")
                        .take();
                    Ok(())
                })
            }),
        )?;
        *active = Some(registration);
        Ok(())
    }
}

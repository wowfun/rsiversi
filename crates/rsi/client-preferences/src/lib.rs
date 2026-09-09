//! Application-lifetime input preferences registered through ordinary Settings.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{
    SettingsAccess, SettingsContract, SettingsError, SettingsSpec, ValidateWith,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// Registered Settings namespace, shared by native and remote clients.
pub const NAMESPACE: &str = "rsi.client";

/// One composer's input preference, independent of interaction-form keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Composer {
    /// Plain Enter submits; false leaves Enter for a newline.
    pub enter_submit: bool,
}
/// Validated input-mode snapshot captured at application startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    /// Web document composer behavior.
    pub web: Composer,
    /// Fullscreen terminal composer behavior.
    pub tui: Composer,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            web: Composer {
                enter_submit: false,
            },
            tui: Composer { enter_submit: true },
        }
    }
}
impl Preferences {
    /// Reads once; omitted registration uses defaults, while errors remain visible.
    pub async fn load(access: &dyn SettingsAccess) -> rsi_settings_protocol::Result<Self> {
        match access.read(NAMESPACE).await {
            Ok(snapshot) => parse(snapshot.value),
            Err(SettingsError::UnknownNamespace(_)) => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }
}
fn parse(value: Value) -> rsi_settings_protocol::Result<Preferences> {
    serde_json::from_value(value).map_err(|error| SettingsError::InvalidInput(error.to_string()))
}

/// Ordinary Settings consumer; dropping its lease removes the namespace owner.
#[derive(Clone, Debug, Default)]
pub struct ClientPreferencesFactory;
#[async_trait]
impl PluginFactory for ClientPreferencesFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Client preferences configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null).requiring_local::<SettingsContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let composer = json!({"type":"object","properties":{"enter_submit":{"type":"boolean"}},"required":["enter_submit"],"additionalProperties":false});
        let registration = plan.local::<SettingsContract>()?.register(SettingsSpec {
            namespace: NAMESPACE.into(),
            defaults: serde_json::to_value(Preferences::default()).expect("closed preferences"),
            base: json!({}),
            metadata: rsi_settings_protocol::SettingsMetadata {
                schema: json!({"type":"object","properties":{"web":composer,"tui":composer},"required":["web","tui"],"additionalProperties":false}),
                applies: rsi_settings_protocol::SettingsApply::Restart,
                description: "Reconnect Web or restart TUI to apply composer Enter behavior. Ctrl/Command+Enter sends in Web; Ctrl+S sends in TUI. Questions and form fields keep their own keys.".into(),
                sensitive_fields: vec![],
            },
            validator: Arc::new(ValidateWith(|value: &Value| parse(value.clone()).map(|_| ()))),
        }).map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "withdraw client preferences",
            Box::new(move || {
                Box::pin(async move {
                    drop(registration);
                    Ok(())
                })
            }),
        )
    }
}

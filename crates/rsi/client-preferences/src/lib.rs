//! Live presentation and input preferences registered through ordinary Settings.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{
    MigrateWith, SettingsAccess, SettingsContract, SettingsError, SettingsSpec, ValidateWith,
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
    /// Keyboard gesture choosing the primary action.
    pub submit_key: SubmitKey,
    /// Preferred delivery while a Turn is active.
    pub busy_submit: BusySubmit,
}
/// Primary submission key preference.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmitKey {
    /// Plain Enter sends; modified Enter selects the alternate busy action.
    #[default]
    Enter,
    /// Modified Enter sends; plain Enter inserts a newline.
    ModEnter,
}
/// Primary delivery while a Turn is running.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusySubmit {
    /// Save for the next Turn.
    #[default]
    Queue,
    /// Enter the next available current step, otherwise queue for a later Turn.
    Steer,
}

fn migrate(raw: Option<&Value>) -> rsi_settings_protocol::Result<Option<Value>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut value = raw.clone();
    if let Some(web) = value.get_mut("web").and_then(Value::as_object_mut) {
        if let Some(old) = web.remove("enter_submit")
            && !old.is_boolean()
        {
            return Err(SettingsError::InvalidInput(
                "Legacy enter_submit must be a boolean".into(),
            ));
        }
        web.entry("submit_key").or_insert(json!("enter"));
        web.entry("busy_submit").or_insert(json!("queue"));
    }
    Ok(Some(value))
}

/// Preferred color scheme; System follows the displaying device.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the device's color-scheme preference.
    #[default]
    System,
    /// Use a light surface.
    Light,
    /// Use a dark surface.
    Dark,
}
/// Profile-wide presentation preferences, validated at the Settings boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Appearance {
    /// Preferred color scheme.
    pub theme: Theme,
    /// Conversation text size in CSS pixels, from 12 through 17.
    pub content_font_size: u8,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            content_font_size: 14,
        }
    }
}
/// Validated preferences snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    /// Web document composer behavior.
    pub web: Composer,
    /// Shared visual presentation.
    pub appearance: Appearance,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            web: Composer {
                submit_key: SubmitKey::Enter,
                busy_submit: BusySubmit::Queue,
            },
            appearance: Appearance::default(),
        }
    }
}
impl Preferences {
    /// Reads once; omitted registration uses defaults, while errors remain visible.
    pub async fn load(access: &dyn SettingsAccess) -> rsi_settings_protocol::Result<Self> {
        match access.read(NAMESPACE).await {
            Ok(snapshot) => Self::from_value(snapshot.value),
            Err(SettingsError::UnknownNamespace(_)) => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }
    /// Validates a complete merged Settings snapshot.
    pub fn from_value(value: Value) -> rsi_settings_protocol::Result<Self> {
        let preferences: Self = serde_json::from_value(value)
            .map_err(|error| SettingsError::InvalidInput(error.to_string()))?;
        if !(12..=17).contains(&preferences.appearance.content_font_size) {
            return Err(SettingsError::InvalidInput(
                "Content font size must be an integer from 12 through 17".into(),
            ));
        }
        Ok(preferences)
    }
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
        let composer = json!({"type":"object","properties":{"submit_key":{"type":"string","enum":["enter","mod_enter"]},"busy_submit":{"type":"string","enum":["queue","steer"]}},"required":["submit_key","busy_submit"],"additionalProperties":false});
        let appearance = json!({"type":"object","properties":{"theme":{"type":"string","enum":["system","light","dark"]},"content_font_size":{"type":"integer","minimum":12,"maximum":17}},"required":["theme","content_font_size"],"additionalProperties":false});
        let registration = plan.local::<SettingsContract>()?.register_migrating(SettingsSpec {
            namespace: NAMESPACE.into(),
            defaults: serde_json::to_value(Preferences::default()).expect("closed preferences"),
            base: json!({}),
            metadata: rsi_settings_protocol::SettingsMetadata {
                schema: json!({"type":"object","properties":{"web":composer,"appearance":appearance},"required":["web","appearance"],"additionalProperties":false}),
                applies: rsi_settings_protocol::SettingsApply::Live,
                description: "GUI preferences apply live across this Profile. Ctrl/Command+Enter sends in Web. Questions and form fields keep their own keys.".into(),
                sensitive_fields: vec![],
            },
            validator: Arc::new(ValidateWith(|value: &Value| Preferences::from_value(value.clone()).map(|_| ()))),
        }, Arc::new(MigrateWith(migrate))).await.map_err(|error| MetaError::Activation(error.to_string()))?;
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

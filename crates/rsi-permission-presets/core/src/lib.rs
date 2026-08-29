//! Frozen exact-name permission preset plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_sandbox::SandboxMode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// Frozen permission preset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionPreset {
    /// File-effect sandbox mode.
    pub sandbox: SandboxMode,
    /// Whether effects require live approval.
    pub require_approval: bool,
}

/// Exact-name preset resolver.
pub trait PermissionPresets: fmt::Debug + Send + Sync + 'static {
    /// Lists frozen presets in exact-name order.
    fn list(&self) -> BTreeMap<String, PermissionPreset>;
    /// Resolves one exact preset.
    fn get(&self, name: &str) -> Result<PermissionPreset>;
}

/// Nominal Local contract for [`PermissionPresets`].
#[derive(Debug)]
pub struct PermissionPresetsContract;

impl LocalContract for PermissionPresetsContract {
    const KEY: &'static str = "rsi.permission_presets";
    type Service = dyn PermissionPresets;
}

/// Closed preset failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PermissionPresetError {
    /// Malformed name/configuration.
    #[error("invalid permission preset: {0}")]
    InvalidInput(String),
    /// Unknown exact name.
    #[error("permission preset `{0}` is not configured")]
    Unknown(String),
}

/// Preset result.
pub type Result<T> = std::result::Result<T, PermissionPresetError>;

#[derive(Debug)]
struct Service {
    presets: BTreeMap<String, PermissionPreset>,
}

impl PermissionPresets for Service {
    fn list(&self) -> BTreeMap<String, PermissionPreset> {
        self.presets.clone()
    }

    fn get(&self, name: &str) -> Result<PermissionPreset> {
        self.presets
            .get(name)
            .cloned()
            .ok_or_else(|| PermissionPresetError::Unknown(name.into()))
    }
}

/// Ordinary factory for one frozen preset map.
#[derive(Clone, Debug, Default)]
pub struct PermissionPresetsFactory;

#[async_trait]
impl PluginFactory for PermissionPresetsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let presets: BTreeMap<String, PermissionPreset> =
            serde_json::from_value(desired.clone())
                .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        if presets.is_empty() || presets.len() > 256 {
            return Err(MetaError::InvalidInput(
                "permission presets must contain 1..=256 entries".into(),
            ));
        }
        let retained = presets.keys().map(String::len).sum();
        for name in presets.keys() {
            if name.is_empty() || name.len() > 256 {
                return Err(MetaError::InvalidInput(
                    "permission preset name is empty or too large".into(),
                ));
            }
        }
        Ok(PreparedActivation::with_state(
            desired.clone(),
            presets,
            retained,
        ))
    }

    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let presets: Arc<dyn PermissionPresets> = Arc::new(Service {
            presets: plan.take_state::<BTreeMap<String, PermissionPreset>>()?,
        });
        let supply = plan
            .context()
            .provide_local::<PermissionPresetsContract>(presets)?;
        plan.defer(
            "withdraw Permission Presets",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

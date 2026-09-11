use async_trait::async_trait;
use rsi_api_protocol::{ApiError, CallOrigin, Result};
use rsi_meta::{ActivationPlan, ConfigValue, LocalContract, PluginFactory, PreparedActivation};
use serde_json::Value;
use std::{fmt, sync::Arc};

/// Retained authority for one admitted Settings mutation.
pub trait SettingsMutationLease: fmt::Debug + Send + Sync + 'static {}
impl<T: fmt::Debug + Send + Sync + 'static> SettingsMutationLease for T {}

/// Application-supplied mutation policy; discovery and reads grant no write authority.
#[async_trait]
pub trait SettingsMutationPolicy: fmt::Debug + Send + Sync + 'static {
    /// Admits one replacement or clear using authenticated transport identity.
    /// `None` means clear; a returned lease lives through durable completion.
    async fn admit(
        &self,
        origin: &CallOrigin,
        namespace: &str,
        replacement: Option<&Value>,
    ) -> Result<Box<dyn SettingsMutationLease>>;
}

/// Nominal Local marker for the application-owned write policy.
#[derive(Debug)]
pub struct SettingsMutationPolicyContract;
impl LocalContract for SettingsMutationPolicyContract {
    const KEY: &'static str = "rsi.settings.api.mutation-policy";
    type Service = dyn SettingsMutationPolicy;
}

/// Explicit standalone policy and ordinary plugin admitting only Local mutations.
#[derive(Clone, Debug, Default)]
pub struct LocalSettingsPolicy;
#[async_trait]
impl SettingsMutationPolicy for LocalSettingsPolicy {
    async fn admit(
        &self,
        origin: &CallOrigin,
        _: &str,
        _: Option<&Value>,
    ) -> Result<Box<dyn SettingsMutationLease>> {
        match origin {
            CallOrigin::Local => Ok(Box::new(())),
            CallOrigin::Device(_) => Err(ApiError::Unauthorized),
        }
    }
}
#[async_trait]
impl PluginFactory for LocalSettingsPolicy {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        crate::prepare(config)
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<SettingsMutationPolicyContract>(Arc::new(Self))?;
        Ok(())
    }
}

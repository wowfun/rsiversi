use crate::wire::{Clear, Operation, Read, Replace, result};
use async_trait::async_trait;
use rsi_api_protocol::{ApiRegistrar, ApiRegistrarContract, ApiRegistration, json_handler};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_settings_protocol::{SettingsAccess, SettingsAccessContract};
use std::sync::Arc;

/// Owns the exact Settings namespace API registrations for one domain generation.
#[derive(Debug)]
pub struct SettingsApi {
    registrations: Vec<ApiRegistration>,
}
impl SettingsApi {
    /// Registers domain handlers using projection authority only.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        settings: Arc<dyn SettingsAccess>,
    ) -> rsi_api_protocol::Result<Self> {
        let mut registrations = Vec::new();
        let service = settings.clone();
        registrations.push(registrar.register(
            Operation::Read.spec(),
            json_handler(move |_, input: Read| {
                let service = service.clone();
                async move { result(service.read(&input.namespace).await) }
            }),
        )?);
        let service = settings.clone();
        registrations.push(registrar.register(
            Operation::Replace.spec(),
            json_handler(move |_, input: Replace| {
                let service = service.clone();
                async move {
                    result(
                        service
                            .replace(&input.namespace, &input.expected, input.value)
                            .await,
                    )
                }
            }),
        )?);
        registrations.push(registrar.register(
            Operation::Clear.spec(),
            json_handler(move |_, input: Clear| {
                let service = settings.clone();
                async move { result(service.clear(&input.namespace, &input.expected).await) }
            }),
        )?);
        Ok(Self { registrations })
    }
    /// Fences all Settings routes and drains already admitted mutations.
    pub async fn close(self) {
        futures_util::future::join_all(self.registrations.into_iter().map(ApiRegistration::close))
            .await;
    }
}

/// Ordinary Settings endpoint plugin with no registration or raw-document authority.
#[derive(Clone, Debug, Default)]
pub struct SettingsApiFactory;
#[async_trait]
impl PluginFactory for SettingsApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(crate::prepare(config)?
            .requiring_local::<SettingsAccessContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let api =
            SettingsApi::register(registrar.as_ref(), plan.local::<SettingsAccessContract>()?)
                .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Settings API",
            Box::new(move || {
                Box::pin(async move {
                    api.close().await;
                    Ok(())
                })
            }),
        )
    }
}

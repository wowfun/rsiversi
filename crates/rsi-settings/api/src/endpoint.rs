use crate::SettingsMutation;
use crate::wire::{Clear, List, Operation, Read, Replace, SetFields, result};
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
        policy: Arc<dyn crate::SettingsMutationPolicy>,
    ) -> rsi_api_protocol::Result<Self> {
        let mut registrations = Vec::new();
        let service = settings.clone();
        registrations.push(registrar.register(
            Operation::List.spec(),
            json_handler(move |_, input: List| {
                let service = service.clone();
                async move { result(service.list(input.after.as_deref(), input.limit).await) }
            }),
        )?);
        let service = settings.clone();
        registrations.push(registrar.register(
            Operation::Describe.spec(),
            json_handler(move |_, input: Read| {
                let service = service.clone();
                async move { result(service.describe(&input.namespace).await) }
            }),
        )?);
        let service = settings.clone();
        registrations.push(registrar.register(
            Operation::Read.spec(),
            json_handler(move |_, input: Read| {
                let service = service.clone();
                async move { result(service.read(&input.namespace).await) }
            }),
        )?);
        let service = settings.clone();
        let replace_policy = policy.clone();
        registrations.push(registrar.register(
            Operation::Replace.spec(),
            json_handler(move |context, input: Replace| {
                let service = service.clone();
                let policy = replace_policy.clone();
                async move {
                    let _admission = policy
                        .admit(
                            &context.origin,
                            &input.namespace,
                            SettingsMutation::Replace(&input.value),
                        )
                        .await?;
                    result(
                        service
                            .replace(&input.namespace, &input.expected, input.value)
                            .await,
                    )
                }
            }),
        )?);
        let service = settings.clone();
        let field_policy = policy.clone();
        registrations.push(registrar.register(
            Operation::SetFields.spec(),
            json_handler(move |context, input: SetFields| {
                let service = service.clone();
                let policy = field_policy.clone();
                async move {
                    let _admission = policy
                        .admit(
                            &context.origin,
                            &input.namespace,
                            SettingsMutation::SetFields(&input.fields),
                        )
                        .await?;
                    result(
                        service
                            .set_fields(&input.namespace, &input.expected, input.fields)
                            .await,
                    )
                }
            }),
        )?);
        registrations.push(registrar.register(
            Operation::Clear.spec(),
            json_handler(move |context, input: Clear| {
                let service = settings.clone();
                let policy = policy.clone();
                async move {
                    let _admission = policy
                        .admit(&context.origin, &input.namespace, SettingsMutation::Clear)
                        .await?;
                    result(service.clear(&input.namespace, &input.expected).await)
                }
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
            .requiring_local::<crate::SettingsMutationPolicyContract>()
            .requiring_local::<SettingsAccessContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let api = SettingsApi::register(
            registrar.as_ref(),
            plan.local::<SettingsAccessContract>()?,
            plan.local::<crate::SettingsMutationPolicyContract>()?,
        )
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

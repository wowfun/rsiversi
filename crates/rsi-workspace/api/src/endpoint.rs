use crate::wire::{IdRequest, ListRequest, Operation, RegisterRequest, result};
use async_trait::async_trait;
use rsi_api_protocol::{ApiRegistrar, ApiRegistrarContract, ApiRegistration, Result, json_handler};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_workspace_protocol::{WorkspaceIngress, WorkspaceIngressContract};
use std::sync::Arc;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// Owns exactly this domain generation's API registrations and admitted work.
#[derive(Debug)]
pub struct WorkspaceApi {
    registrations: Vec<ApiRegistration>,
}
impl WorkspaceApi {
    /// Registers domain operations without involving a transport or Session implementation.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        workspace: Arc<dyn WorkspaceIngress>,
    ) -> Result<Self> {
        let mut registrations = Vec::new();
        let service = workspace.clone();
        registrations.push(registrar.register(
            Operation::OrderSeed.spec(),
            json_handler(move |context, _: Empty| {
                let service = service.scoped(context.origin);
                async move { result(service.order_seed().await) }
            }),
        )?);
        let service = workspace.clone();
        registrations.push(registrar.register(
            Operation::Get.spec(),
            json_handler(move |context, input: IdRequest| {
                let service = service.scoped(context.origin);
                async move { result(service.get(&input.id).await) }
            }),
        )?);
        let service = workspace.clone();
        registrations.push(registrar.register(
            Operation::List.spec(),
            json_handler(move |context, input: ListRequest| {
                let service = service.scoped(context.origin);
                async move { result(service.list(input.after, input.limit).await) }
            }),
        )?);
        let service = workspace.clone();
        registrations.push(registrar.register(
            Operation::Register.spec(),
            json_handler(move |context, input: RegisterRequest| {
                let service = service.scoped(context.origin);
                async move { result(service.register_at(&input.location, &input.path).await) }
            }),
        )?);
        let service = workspace.clone();
        registrations.push(registrar.register(
            Operation::Status.spec(),
            json_handler(move |context, input: IdRequest| {
                let service = service.scoped(context.origin);
                async move { result(service.status(&input.id).await) }
            }),
        )?);
        registrations.push(registrar.register(
            Operation::Delete.spec(),
            json_handler(move |context, input: IdRequest| {
                let service = workspace.scoped(context.origin);
                async move { result(service.delete_registration(&input.id).await) }
            }),
        )?);
        Ok(Self { registrations })
    }
    /// Retires all domain operations and drains their owned mutations.
    pub async fn close(self) {
        futures_util::future::join_all(self.registrations.into_iter().map(ApiRegistration::close))
            .await;
    }
}

/// Ordinary Meta endpoint owner requiring only Workspace and the generic registrar.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceApiFactory;
#[async_trait]
impl PluginFactory for WorkspaceApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        crate::empty(config)?;
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<WorkspaceIngressContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let api = WorkspaceApi::register(
            registrar.as_ref(),
            plan.local::<WorkspaceIngressContract>()?,
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        plan.defer(
            "retire Workspace API",
            Box::new(move || {
                Box::pin(async move {
                    api.close().await;
                    Ok(())
                })
            }),
        )
    }
}

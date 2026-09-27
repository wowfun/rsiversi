use async_trait::async_trait;
use rsi_api::{ApiRegistry, ConnectionApi};
use rsi_api_protocol::{
    ApiDispatch, ApiDispatchContract, ApiError, ApiInvocation, CallOrigin,
    ConnectionDescriptionContract, OperationCatalog, OperationId, OperationSpec, caller_operation,
    describe_operation, operations_operation,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
    ResolvedFactory, UpdateMode,
};
use rsi_web_assets::WebAssetControlContract;
use rsi_web_assets_api::WebAssetsApi;
use std::sync::Arc;

#[derive(Debug)]
struct Dispatch {
    service: Arc<dyn ApiDispatch>,
    local: Arc<ApiRegistry>,
}
impl ApiDispatch for Dispatch {
    fn admit(
        &self,
        operation: &OperationId,
        origin: CallOrigin,
    ) -> rsi_api_protocol::Result<Box<dyn ApiInvocation>> {
        if self
            .local
            .operations()
            .iter()
            .any(|spec| &spec.id == operation)
        {
            self.local.admit(operation, origin)
        } else {
            self.service.admit(operation, origin)
        }
    }
    fn operations(&self) -> Vec<OperationSpec> {
        let mut operations = self.service.operations();
        operations.retain(|spec| {
            spec.id != describe_operation().id
                && spec.id != operations_operation().id
                && spec.id != caller_operation().id
        });
        operations.extend(self.local.operations());
        // A later Service generation cannot silently steal Application operations.
        OperationCatalog::new(operations)
            .map_or_else(|_| Vec::new(), |catalog| catalog.operations().to_vec())
    }
}
fn failure(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

pub(super) async fn prepare(
    plan: &mut ActivationPlan,
    context: Context,
) -> rsi_meta::Result<Context> {
    let factory = WebApiFactory {
        service: plan.local::<ApiDispatchContract>()?,
        description: plan.local::<ConnectionDescriptionContract>()?,
        assets: plan.local::<WebAssetControlContract>()?,
    };
    let context = context.isolate_local_fresh::<ApiDispatchContract>()?.0;
    let child = context
        .apply(
            ResolvedFactory::linked(
                "rsi.serve.web-api",
                env!("CARGO_PKG_VERSION"),
                UpdateMode::RestartRequired,
                Arc::new(factory),
            ),
            ConfigValue::Null,
        )
        .await?;
    if child.snapshot().state != rsi_meta::FiberState::Active {
        return Err(failure(format!(
            "Web API activation failed: {:?}",
            child.snapshot().state
        )));
    }
    Ok(context)
}

// Inputs come from the requiring Serve generation; its children retire before
// those parent dependency leases. This factory publishes into an isolated slot.
#[derive(Debug)]
struct WebApiFactory {
    service: Arc<dyn ApiDispatch>,
    description: Arc<rsi_api_protocol::ConnectionDescription>,
    assets: Arc<rsi_web_assets::WebAssetControl>,
}
#[async_trait]
impl PluginFactory for WebApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Web API accepts null configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let context = plan.context();
        let service = self.service.clone();
        if service.operations().iter().any(|spec| {
            rsi_web_assets_api::operations()
                .iter()
                .any(|asset| asset.id == spec.id)
        }) {
            return Err(failure(ApiError::Invalid(
                "Service already owns a Web asset operation".into(),
            )));
        }
        let execution = context.runtime().execution().clone();
        let registry = Arc::new(ApiRegistry::new(execution.clone()));
        let retiring = registry.clone();
        plan.defer(
            "retire Serve Web API registry",
            Box::new(move || {
                Box::pin(async move {
                    retiring.close().await;
                    Ok(())
                })
            }),
        )?;
        let assets = WebAssetsApi::register(registry.as_ref(), execution, self.assets.clone())
            .map_err(failure)?;
        plan.defer(
            "join Serve Web asset leases",
            Box::new(move || {
                Box::pin(async move {
                    assets.close().await;
                    Ok(())
                })
            }),
        )?;
        let dispatch = Arc::new(Dispatch {
            service,
            local: registry.clone(),
        });
        let description = &self.description;
        let connection = ConnectionApi::register(
            dispatch.clone(),
            registry.as_ref(),
            description.endpoint_id.clone(),
            description.host_epoch.clone(),
        )
        .map_err(failure)?;
        if dispatch.operations().is_empty() {
            return Err(failure("invalid combined Web operation catalog"));
        }
        plan.defer(
            "retire Serve Web negotiation",
            Box::new(move || {
                Box::pin(async move {
                    connection.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = context.provide_local::<ApiDispatchContract>(dispatch)?;
        plan.defer(
            "withdraw Serve Web dispatch",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )?;
        Ok(())
    }
}

use async_trait::async_trait;
use rsi_api::{ApiRegistry, ConnectionApi};
use rsi_api_protocol::{
    ApiDispatch, ApiDispatchContract, ApiInvocation, CallOrigin, ConnectionDescriptionContract,
    OperationCall, OperationSpec, caller_operation, describe_operation, operation_operation,
    operations_operation,
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
impl Dispatch {
    fn owns(id: &rsi_api_protocol::OperationId) -> bool {
        [
            describe_operation(),
            operations_operation(),
            operation_operation(),
            caller_operation(),
        ]
        .into_iter()
        .chain(rsi_web_assets_api::operations())
        .any(|spec| &spec.id == id)
    }
}
impl ApiDispatch for Dispatch {
    fn admit(
        &self,
        operation: &OperationCall,
        origin: CallOrigin,
    ) -> rsi_api_protocol::Result<Box<dyn ApiInvocation>> {
        if Self::owns(&operation.key.id) {
            self.local.admit(operation, origin)
        } else {
            self.service.admit(operation, origin)
        }
    }
    fn operation(
        &self,
        key: &rsi_api_protocol::OperationKey,
        origin: &CallOrigin,
    ) -> rsi_api_protocol::Result<OperationSpec> {
        if Self::owns(&key.id) {
            self.local.operation(key, origin)
        } else {
            self.service.operation(key, origin)
        }
    }
    fn input_budget(
        &self,
        class: rsi_api_protocol::OperationClass,
    ) -> rsi_api_protocol::ByteBudget {
        self.service.input_budget(class)
    }
    fn operations(&self) -> rsi_api_protocol::Result<Vec<OperationSpec>> {
        let local = self.local.operations()?;
        let mut operations = self.service.operations()?;
        operations.retain(|spec| !Self::owns(&spec.id));
        operations.extend(local);
        Ok(operations)
    }
}
fn failure(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_api_protocol::{ApiHandler, ApiRegistrar, OperationCatalog};

    #[derive(Debug)]
    struct Unused;
    #[async_trait]
    impl ApiHandler for Unused {
        async fn invoke(
            &self,
            _: rsi_api_protocol::ApiContext,
            _: rsi_api_protocol::RetainedBytes,
            _: rsi_api_protocol::ApiResponseCapacity,
        ) -> rsi_api_protocol::Result<rsi_api_protocol::ApiOutput> {
            unreachable!("catalog test does not invoke handlers")
        }
    }

    #[tokio::test]
    async fn later_service_collision_preserves_local_policy_and_unrelated_discovery() {
        let execution = rsi_meta::Execution::native(tokio::runtime::Handle::current());
        let service = Arc::new(ApiRegistry::new(execution.clone()));
        let local = Arc::new(ApiRegistry::new(execution));
        let local_spec = rsi_web_assets_api::operations()[0].clone();
        let local_guard = local
            .register(local_spec.clone(), Arc::new(Unused))
            .unwrap();
        let mut other = local_spec.clone();
        other.id = rsi_api_protocol::OperationId::new("test", "unrelated", 1).unwrap();
        let other_guard = service.register(other.clone(), Arc::new(Unused)).unwrap();
        let dispatch = Dispatch {
            service: service.clone(),
            local: local.clone(),
        };
        assert_eq!(dispatch.operations().unwrap().len(), 2);
        let mut conflicting = local_spec.clone();
        conflicting.maximum_request_bytes += 1;
        let collision = service.register(conflicting, Arc::new(Unused)).unwrap();
        let catalog = OperationCatalog::new(dispatch.operations().unwrap()).unwrap();
        assert_eq!(catalog.operations().len(), 2);
        assert!(catalog.operations().contains(&local_spec));
        assert!(catalog.operations().contains(&other));
        assert_eq!(
            dispatch
                .operation(&local_spec.key(), &CallOrigin::Local)
                .unwrap(),
            local_spec
        );
        let admitted = dispatch
            .admit(&local_spec.call(), CallOrigin::Local)
            .unwrap();
        drop(local_guard);
        assert!(matches!(
            dispatch.operation(&local_spec.key(), &CallOrigin::Local),
            Err(rsi_api_protocol::ApiError::OperationRetiring)
        ));
        assert!(matches!(
            dispatch.admit(&local_spec.call(), CallOrigin::Local),
            Err(rsi_api_protocol::ApiError::OperationRetiring)
        ));
        assert!(
            !dispatch
                .operations()
                .unwrap()
                .iter()
                .any(|spec| spec.key() == local_spec.key())
        );
        drop(admitted);
        local.close().await;
        assert!(matches!(
            dispatch.operation(&local_spec.key(), &CallOrigin::Local),
            Err(rsi_api_protocol::ApiError::ShuttingDown)
        ));
        collision.close().await;
        other_guard.close().await;
    }
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
pub(super) struct WebApiFactory {
    pub service: Arc<dyn ApiDispatch>,
    pub description: Arc<rsi_api_protocol::ConnectionDescription>,
    pub assets: Arc<rsi_web_assets::WebAssetControl>,
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

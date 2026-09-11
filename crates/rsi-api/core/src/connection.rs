use async_trait::async_trait;
use rsi_api_protocol::{
    ApiContext, ApiDispatch, ApiDispatchContract, ApiError, ApiHandler, ApiMessage, ApiOutput,
    ApiRegistrar, ApiRegistrarContract, ApiRegistration, ApiResponseCapacity, CallOrigin,
    CallerIdentity, ConnectionDescription, ConnectionDescriptionContract, ConnectionHello,
    EndpointId, EndpointIdentityContract, HostEpoch, HostGenerationContract, OperationCatalog,
    Result, RetainedBytes, caller_operation, describe_operation, operations_operation,
};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use serde::Deserialize;
use std::sync::Arc;

/// Transport-independent owner of one generation's negotiation operations.
#[derive(Debug)]
pub struct ConnectionApi {
    description: Arc<ConnectionDescription>,
    registrations: Vec<ApiRegistration>,
}
impl ConnectionApi {
    /// Registers the bounded description and catalog on the exact supplied registry.
    pub fn register(
        dispatch: Arc<dyn ApiDispatch>,
        registrar: &dyn ApiRegistrar,
        endpoint_id: EndpointId,
        host_epoch: HostEpoch,
    ) -> Result<Self> {
        let description = Arc::new(ConnectionDescription {
            wire_version: 1,
            endpoint_id,
            host_epoch,
        });
        let describe = registrar.register(
            describe_operation(),
            Arc::new(Describe((*description).clone())),
        )?;
        let operations =
            registrar.register(operations_operation(), Arc::new(Operations(dispatch)))?;
        let caller = registrar.register(caller_operation(), Arc::new(Caller))?;
        Ok(Self {
            description,
            registrations: vec![describe, operations, caller],
        })
    }
    /// Clones the immutable identity shared by all listeners of this generation.
    pub fn description(&self) -> Arc<ConnectionDescription> {
        self.description.clone()
    }
    /// Fences and drains the registrations without retiring independent domain services.
    pub async fn close(self) {
        for registration in self.registrations {
            registration.close().await;
        }
    }
}

/// Ordinary connection API owner, independent of any listener or client transport.
#[derive(Clone, Debug, Default)]
pub struct ConnectionApiFactory;
#[async_trait]
impl PluginFactory for ConnectionApiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "connection API configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiDispatchContract>()
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<EndpointIdentityContract>()
            .requiring_local::<HostGenerationContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let connection = ConnectionApi::register(
            plan.local::<ApiDispatchContract>()?,
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            (*plan.local::<EndpointIdentityContract>()?).clone(),
            (*plan.local::<HostGenerationContract>()?).clone(),
        )
        .map_err(|error| MetaError::Activation(error.to_string()))?;
        let description = connection.description();
        plan.defer(
            "drain connection API",
            Box::new(move || {
                Box::pin(async move {
                    connection.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan
            .context()
            .provide_local::<ConnectionDescriptionContract>(description)?;
        plan.defer(
            "withdraw connection description",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

#[derive(Debug)]
struct Describe(ConnectionDescription);
#[async_trait]
impl ApiHandler for Describe {
    async fn invoke(
        &self,
        _: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let hello: ConnectionHello = serde_json::from_slice(input.as_bytes())
            .map_err(|_| ApiError::Invalid("invalid handshake".into()))?;
        if hello.wire_version != 1
            || hello
                .expected_endpoint
                .as_ref()
                .is_some_and(|id| id != &self.0.endpoint_id)
        {
            return Err(ApiError::Unavailable);
        }
        let ApiResponseCapacity::Finite(reservation) = output else {
            return Err(ApiError::Backend(
                "invalid connection response class".into(),
            ));
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: reservation.encode(&self.0)?,
            binary: None,
        }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Debug)]
struct Caller;
#[async_trait]
impl ApiHandler for Caller {
    async fn invoke(
        &self,
        context: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let _: Empty = serde_json::from_slice(input.as_bytes())
            .map_err(|_| ApiError::Invalid("caller expects an empty object".into()))?;
        let identity = match context.origin {
            CallOrigin::Local => CallerIdentity::Local,
            CallOrigin::Device(device) => CallerIdentity::Device {
                device_id: device.id,
            },
        };
        let ApiResponseCapacity::Finite(reservation) = output else {
            return Err(ApiError::Backend(
                "invalid connection response class".into(),
            ));
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: reservation.encode(&identity)?,
            binary: None,
        }))
    }
}

#[derive(Debug)]
struct Operations(Arc<dyn ApiDispatch>);
#[async_trait]
impl ApiHandler for Operations {
    async fn invoke(
        &self,
        context: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        let _: Empty = serde_json::from_slice(input.as_bytes())
            .map_err(|_| ApiError::Invalid("operations expects an empty object".into()))?;
        let ApiResponseCapacity::Finite(reservation) = output else {
            return Err(ApiError::Backend(
                "invalid connection response class".into(),
            ));
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: reservation.encode(&OperationCatalog::new(
                self.0
                    .operations()
                    .into_iter()
                    .filter(|operation| operation.access.permits(&context.origin))
                    .collect(),
            )?)?,
            binary: None,
        }))
    }
}

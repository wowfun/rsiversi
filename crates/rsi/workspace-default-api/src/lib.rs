//! First-use workspace initialization without model or Session creation.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiError, OperationAccess, OperationClass, OperationEffect,
    OperationId, OperationSpec, RequestEncoding, call_json,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, MetaError, PluginFactory, PreparedActivation,
};
use rsi_workspace_protocol::WorkspaceRecord;
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// Closed empty initialization request.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initialize {}

/// One bounded, idempotent mutation owned by the standard Service.
#[expect(
    clippy::missing_panics_doc,
    reason = "the fixed operation identifier is valid"
)]
pub fn operation() -> OperationSpec {
    OperationSpec {
        protocol: rsi_api_protocol::wire_contract!(),
        id: OperationId::new("workspace-default", "initialize", 1).expect("static operation"),
        access: OperationAccess::Authenticated,
        class: OperationClass::Data,
        effect: OperationEffect::Mutation,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 128,
        maximum_response_bytes: 128 * 1024,
    }
}

/// Shared native/Worker client.
#[derive(Debug)]
pub struct Client {
    api: Arc<dyn ApiClient>,
}
impl Client {
    /// Creates a proxy for one negotiated connection.
    pub fn new(api: Arc<dyn ApiClient>) -> Self {
        Self { api }
    }
    /// Initializes once or returns the existing default registration.
    pub async fn initialize(&self) -> rsi_api_protocol::Result<Option<WorkspaceRecord>> {
        let record = call_json::<_, Option<WorkspaceRecord>, String>(
            self.api.as_ref(),
            &operation(),
            &Initialize {},
        )
        .await?
        .map_err(|message| {
            ApiError::Backend(if message.len() <= 512 {
                message
            } else {
                "Oversized initialization diagnostic".into()
            })
        })?;
        if let Some(record) = &record {
            record
                .validate()
                .map_err(|_| ApiError::Invalid("Invalid default workspace identity".into()))?;
            if !matches!(
                record.coordinates.location(),
                rsi_workspace_protocol::ExecutionLocation::Local
            ) {
                return Err(ApiError::Invalid(
                    "Default workspace must be local to its Service".into(),
                ));
            }
        }
        Ok(record)
    }
}
/// Nominal client capability.
#[derive(Debug)]
pub struct ClientContract;
impl LocalContract for ClientContract {
    const KEY: &'static str = "rsi.workspace-default.client";
    type Service = Client;
}
/// Ordinary shared API client plugin.
#[derive(Clone, Debug, Default)]
pub struct ClientFactory;
#[async_trait]
impl PluginFactory for ClientFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Default workspace client config must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<ClientContract>(Arc::new(Client::new(
                plan.local::<ApiClientContract>()?,
            )))?;
        plan.defer(
            "withdraw default workspace client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
/// Descriptor for negotiated application admission.
pub fn protocol_descriptor() -> rsi_api_protocol::ProtocolDescriptor {
    rsi_api_protocol::wire_contract!()
}

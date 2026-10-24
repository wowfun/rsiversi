use crate::transport::LocalTransport;
use async_trait::async_trait;
use rsi_api_client::ClientConnection;
use rsi_api_protocol::{
    ApiClient, ApiClientContract, ApiError, ApiOutput, ByteBudget, ConnectionDescription,
    EndpointId, HostEpoch, LocalCompatibilityKey, OperationClass, OperationSpec, Result,
    RetainedBytes,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, MetaError, PluginFactory, PreparedActivation,
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// Explicit native endpoint and exact deployment-selection fences.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UdsClientConfig {
    /// Absolute private Unix socket path selected by the product owner.
    pub socket: PathBuf,
    /// Expected persisted deployment identity.
    pub endpoint_id: EndpointId,
    /// Expected running generation, checked before client publication.
    pub host_epoch: HostEpoch,
    /// Opaque local build/launch fence derived by the product.
    pub compatibility: LocalCompatibilityKey,
    /// Expected Windows server PID, verified on every native connection.
    #[cfg(windows)]
    pub process_id: u32,
    /// Expected Windows process creation time, fencing PID reuse.
    #[cfg(windows)]
    pub process_start_token: String,
}
impl UdsClientConfig {
    /// Rejects unusable or unbounded local socket addresses before connecting.
    pub fn validate(&self) -> Result<()> {
        #[cfg(unix)]
        {
            if !self.socket.is_absolute() {
                return Err(ApiError::Invalid(
                    "Unix socket path must be absolute".into(),
                ));
            }
            std::os::unix::net::SocketAddr::from_pathname(&self.socket)
                .map(|_| ())
                .map_err(|error| ApiError::Invalid(format!("invalid Unix socket path: {error}")))
        }
        #[cfg(windows)]
        {
            if self.process_id == 0
                || self.process_start_token.is_empty()
                || self.process_start_token.len() > 20
                || !self
                    .process_start_token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
            {
                return Err(ApiError::Invalid("invalid local process identity".into()));
            }
            rsi_api_local_transport::PipeName::new(&self.socket)
                .map(|_| ())
                .map_err(|error| ApiError::Invalid(error.to_string()))
        }
    }
}

/// Shared client owner using one same-UID Unix stream per API exchange.
#[derive(Debug)]
pub struct UdsClient(
    ClientConnection,
    #[cfg(feature = "test-support")] Arc<crate::measurement::Measurement>,
);
impl UdsClient {
    /// Negotiates the selected deployment before any domain call can be issued.
    pub async fn connect(execution: Execution, config: UdsClientConfig) -> Result<Self> {
        config.validate()?;
        let deadline = execution.deadline_after(Duration::from_secs(15));
        let expected = config.host_epoch.clone();
        let endpoint = config.endpoint_id.clone();
        #[cfg(feature = "test-support")]
        let measurement = Arc::new(crate::measurement::Measurement::default());
        let transport = Arc::new(LocalTransport {
            #[cfg(feature = "test-support")]
            measurement: measurement.clone(),
            config,
            execution: execution.clone(),
        });
        let connection = deadline
            .timeout(ClientConnection::connect(execution, endpoint, transport))
            .await
            .map_err(|_| ApiError::Backend("local API negotiation deadline elapsed".into()))??;
        if connection.description().host_epoch != expected {
            connection.close().await;
            return Err(ApiError::ShuttingDown);
        }
        Ok(Self(
            connection,
            #[cfg(feature = "test-support")]
            measurement,
        ))
    }
    /// Drains report-only timing samples; storage is capped at 4096 exchanges.
    #[cfg(feature = "test-support")]
    pub fn take_measurements(&self) -> Vec<crate::ExchangeTiming> {
        self.1.take()
    }
    /// Fences local work without acquiring server shutdown authority.
    pub fn retire(&self) {
        self.0.retire();
    }
    /// Cancels and drains local waiters and observations, including unpolled streams.
    pub async fn close(&self) {
        self.0.close().await;
    }
}
#[async_trait]
impl ApiClient for UdsClient {
    fn description(&self) -> &ConnectionDescription {
        self.0.description()
    }
    async fn operations(&self) -> Result<rsi_api_protocol::OperationCatalog> {
        self.0.operations().await
    }
    async fn operation(&self, key: &rsi_api_protocol::OperationKey) -> Result<OperationSpec> {
        self.0.operation(key).await
    }
    fn input_budget(&self, class: OperationClass) -> ByteBudget {
        self.0.input_budget(class)
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        self.0.call(operation, input).await
    }
}

/// Ordinary native owner of the selected local API connection.
#[derive(Clone, Debug, Default)]
pub struct UdsClientFactory;
#[async_trait]
impl PluginFactory for UdsClientFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: UdsClientConfig = serde_json::from_value(desired.clone())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        config
            .validate()
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let bytes = std::mem::size_of::<UdsClientConfig>() + config.socket.as_os_str().len() + 128;
        Ok(PreparedActivation::with_state(
            desired.clone(),
            config,
            bytes,
        ))
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<UdsClientConfig>()?;
        let client = Arc::new(
            UdsClient::connect(plan.context().runtime().execution().clone(), config)
                .await
                .map_err(|error| MetaError::Activation(error.to_string()))?,
        );
        let cleanup = client.clone();
        plan.defer(
            "drain local API client",
            Box::new(move || {
                Box::pin(async move {
                    cleanup.close().await;
                    Ok(())
                })
            }),
        )?;
        let supply = plan.context().provide_local::<ApiClientContract>(client)?;
        plan.defer(
            "withdraw local API client",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

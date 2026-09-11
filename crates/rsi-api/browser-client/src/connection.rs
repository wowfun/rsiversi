use crate::{BrowserClientConfig, BrowserResourceSnapshot, transport::BrowserTransport};
use async_trait::async_trait;
use rsi_api_client::ClientConnection;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiOutput, ByteBudget, CallerIdentity, ConnectionDescription, DeviceId,
    OperationClass, OperationSpec, Result, RetainedBytes, caller_operation,
};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Same-origin Fetch connection with shared Rust state and explicit platform cleanup.
#[derive(Debug)]
pub struct BrowserClient {
    connection: ClientConnection,
    transport: Arc<BrowserTransport>,
    device_id: DeviceId,
}
impl BrowserClient {
    /// Negotiates with the current `HttpOnly` cookie before publishing a client.
    pub async fn connect(execution: Execution, config: BrowserClientConfig) -> Result<Self> {
        let transport = Arc::new(BrowserTransport::new(execution.clone(), &config)?);
        Self::negotiate(execution, config, transport).await
    }
    pub(crate) async fn negotiate(
        execution: Execution,
        config: BrowserClientConfig,
        transport: Arc<BrowserTransport>,
    ) -> Result<Self> {
        let connection =
            match ClientConnection::connect(execution, config.endpoint_id, transport.clone()).await
            {
                Ok(connection) => connection,
                Err(error) => {
                    transport.close().await?;
                    return Err(error);
                }
            };
        let caller = async {
            let operation = caller_operation();
            let input = connection.input_budget(operation.class).copy(b"{}")?;
            let ApiOutput::Reply(reply) = connection.call(&operation, input).await? else {
                return Err(ApiError::Invalid(
                    "caller identity requires a finite reply".into(),
                ));
            };
            if reply.binary.is_some() {
                return Err(ApiError::Invalid(
                    "caller identity cannot contain binary data".into(),
                ));
            }
            let identity: CallerIdentity = serde_json::from_slice(reply.json.as_bytes())
                .map_err(|_| ApiError::Invalid("invalid authenticated caller identity".into()))?;
            match identity {
                CallerIdentity::Device { device_id } => Ok(device_id),
                CallerIdentity::Local => Err(ApiError::Unauthorized),
            }
        }
        .await;
        match caller {
            Ok(device_id) => {
                transport.pin_device(device_id.clone());
                Ok(Self {
                    connection,
                    transport,
                    device_id,
                })
            }
            Err(error) => {
                connection.close().await;
                transport.close().await?;
                Err(error)
            }
        }
    }
    /// Exact non-secret device identity authenticated before publication.
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }
    /// Exchanges an explicit device token for the server's `HttpOnly` cookie once.
    /// Replacing an existing cookie requires an explicit logout first.
    pub async fn login(
        execution: Execution,
        config: &BrowserClientConfig,
        token: SecretValue,
    ) -> Result<()> {
        if token.expose_secret().len() != 64
            || !token
                .expose_secret()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ApiError::Unauthorized);
        }
        let authorization = Zeroizing::new(format!("Bearer {}", token.expose_secret()));
        drop(token);
        let transport = BrowserTransport::new(execution, config)?;
        let result = transport
            .cookie(&config.endpoint_id, Some(authorization))
            .await;
        transport.close().await?;
        result
    }
    /// Explicitly clears this origin's device cookie without stopping the deployment.
    pub async fn logout(
        execution: Execution,
        config: &BrowserClientConfig,
        device: &DeviceId,
    ) -> Result<()> {
        let transport = BrowserTransport::new(execution, config)?;
        transport.pin_device(device.clone());
        let result = transport.cookie(&config.endpoint_id, None).await;
        transport.close().await?;
        result
    }
    /// Fences local admission and signals browser I/O cancellation.
    pub fn retire(&self) {
        self.connection.retire();
        self.transport.bridge.retire();
    }
    /// Awaits logical work and platform promise settlement; timeout is a cleanup failure.
    pub async fn close(&self) -> Result<()> {
        self.retire();
        self.connection.close().await;
        self.transport.close().await
    }
    /// Observes explicitly owned bridge resources without creating work.
    pub fn resource_snapshot(&self) -> BrowserResourceSnapshot {
        self.transport.bridge.snapshot()
    }
}
impl Drop for BrowserClient {
    fn drop(&mut self) {
        self.retire();
    }
}
#[async_trait]
impl ApiClient for BrowserClient {
    fn description(&self) -> &ConnectionDescription {
        self.connection.description()
    }
    fn operations(&self) -> &[OperationSpec] {
        self.connection.operations()
    }
    fn input_budget(&self, class: OperationClass) -> ByteBudget {
        self.connection.input_budget(class)
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        self.connection.call(operation, input).await
    }
}

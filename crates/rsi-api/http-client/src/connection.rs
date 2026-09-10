use crate::{HttpClientConfig, transport::NativeTransport};
use async_trait::async_trait;
use rsi_api_client::ClientConnection;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiOutput, ByteBudget, ConnectionDescription, OperationClass,
    OperationSpec, Result, RetainedBytes,
};
use rsi_credentials_protocol::SecretValue;
use rsi_meta::Execution;
use std::{sync::Arc, time::Duration};

/// Native authenticated HTTP transport with shared negotiated connection ownership.
#[derive(Debug)]
pub struct HttpClient {
    connection: ClientConnection,
}
impl HttpClient {
    /// Resolves native transport policy and negotiates before publishing the client.
    pub async fn connect(
        execution: Execution,
        config: HttpClientConfig,
        token: SecretValue,
    ) -> Result<Self> {
        config.validate()?;
        if token.expose_secret().len() != 64
            || !token
                .expose_secret()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ApiError::Unauthorized);
        }
        let authorization_text =
            zeroize::Zeroizing::new(format!("Bearer {}", token.expose_secret()));
        let mut authorization =
            http::HeaderValue::from_str(&authorization_text).map_err(|_| ApiError::Unauthorized)?;
        authorization.set_sensitive(true);
        drop(authorization_text);
        drop(token);
        execution
            .deadline_after(Duration::from_secs(15))
            .timeout(async move {
                let transport = Arc::new(NativeTransport {
                    transport: config.transport().await?,
                    execution: execution.clone(),
                    origin: config.origin,
                    authorization,
                });
                let connection =
                    ClientConnection::connect(execution, config.endpoint_id, transport).await?;
                Ok(Self { connection })
            })
            .await
            .map_err(|_| ApiError::Backend("API negotiation deadline elapsed".into()))?
    }
    /// Fences local admission and observations without stopping the remote deployment.
    pub fn retire(&self) {
        self.connection.retire();
    }
    /// Fences admission and drains local exchanges, including unpolled observations.
    pub async fn close(&self) {
        self.connection.close().await;
    }
}
#[async_trait]
impl ApiClient for HttpClient {
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

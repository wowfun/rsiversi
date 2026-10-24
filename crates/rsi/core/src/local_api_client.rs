use rsi_api_protocol::{
    ApiClient, ApiError, ApiOutput, ByteBudget, CallOrigin, ConnectionDescription, OperationClass,
    OperationSpec, RetainedBytes,
};
use std::sync::{Arc, Weak};

/// Typed in-process adapter; Service lifetime and mutation ownership remain upstream.
#[derive(Debug)]
struct LocalClient {
    running: Weak<crate::RunningRsi>,
    description: Arc<ConnectionDescription>,
    inputs: ByteBudget,
}
pub(crate) fn client(running: &Arc<crate::RunningRsi>) -> crate::Result<Arc<dyn ApiClient>> {
    Ok(Arc::new(LocalClient {
        running: Arc::downgrade(running),
        description: running.connection_description()?,
        inputs: ByteBudget::new(64 * 1024 * 1024)
            .map_err(|error| crate::RsiError::Boot(error.to_string()))?,
    }))
}
#[async_trait::async_trait]
impl ApiClient for LocalClient {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        let running = self.running.upgrade().ok_or(ApiError::ShuttingDown)?;
        rsi_api_protocol::OperationCatalog::new(
            running
                .api_dispatch()
                .map_err(|_| ApiError::Unavailable)?
                .operations()?,
        )
    }
    async fn operation(
        &self,
        key: &rsi_api_protocol::OperationKey,
    ) -> rsi_api_protocol::Result<OperationSpec> {
        let running = self.running.upgrade().ok_or(ApiError::ShuttingDown)?;
        running
            .api_dispatch()
            .map_err(|_| ApiError::Unavailable)?
            .operation(key, &CallOrigin::Local)
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.inputs.clone()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let running = self.running.upgrade().ok_or(ApiError::ShuttingDown)?;
        let dispatch = running.api_dispatch().map_err(|_| ApiError::Unavailable)?;
        dispatch
            .admit(&operation.call(), CallOrigin::Local)?
            .invoke(input)
            .await
    }
}

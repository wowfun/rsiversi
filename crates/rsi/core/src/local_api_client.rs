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
    operations: Vec<OperationSpec>,
    inputs: ByteBudget,
}
pub(crate) fn client(running: &Arc<crate::RunningRsi>) -> crate::Result<Arc<dyn ApiClient>> {
    Ok(Arc::new(LocalClient {
        running: Arc::downgrade(running),
        description: running.connection_description()?,
        operations: running.api_dispatch()?.operations(),
        inputs: ByteBudget::new(64 * 1024 * 1024)
            .map_err(|error| crate::RsiError::Boot(error.to_string()))?,
    }))
}
#[async_trait::async_trait]
impl ApiClient for LocalClient {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.inputs.clone()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        if !self.operations.contains(operation) {
            return Err(ApiError::Unavailable);
        }
        let running = self.running.upgrade().ok_or(ApiError::ShuttingDown)?;
        let dispatch = running.api_dispatch().map_err(|_| ApiError::Unavailable)?;
        dispatch
            .admit(&operation.id, CallOrigin::Local)?
            .invoke(input)
            .await
    }
}

use crate::{FilesOperation, SessionFilesClient};
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiDispatch, ApiError, ApiOutput, ByteBudget, CallOrigin, ConnectionDescription,
    OperationClass, OperationSpec, Result, RetainedBytes,
};
use std::sync::Arc;

/// Private trusted adapter: only the four Files reads are reachable, with the
/// same registry-owned input pool and invocation lifecycle as remote requests.
#[derive(Debug)]
struct LocalReads {
    dispatch: Arc<dyn ApiDispatch>,
    description: Arc<ConnectionDescription>,
    operations: Vec<rsi_api_protocol::OperationKey>,
    input: ByteBudget,
}

pub(crate) fn client(
    dispatch: Arc<dyn ApiDispatch>,
    description: Arc<ConnectionDescription>,
) -> SessionFilesClient {
    let operations = [
        FilesOperation::Open,
        FilesOperation::Read,
        FilesOperation::List,
        FilesOperation::Release,
    ]
    .map(|op| op.spec().key())
    .to_vec();
    let input = dispatch.input_budget(OperationClass::Data);
    SessionFilesClient::new(Arc::new(LocalReads {
        dispatch,
        description,
        operations,
        input,
    }))
}

#[async_trait]
impl ApiClient for LocalReads {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> Result<rsi_api_protocol::OperationCatalog> {
        rsi_api_protocol::OperationCatalog::new(
            self.dispatch
                .operations()?
                .into_iter()
                .filter(|spec| self.operations.contains(&spec.key()))
                .collect(),
        )
    }
    async fn operation(&self, key: &rsi_api_protocol::OperationKey) -> Result<OperationSpec> {
        if !self.operations.contains(key) {
            return Err(ApiError::Unauthorized);
        }
        self.dispatch.operation(key, &CallOrigin::Local)
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.input.clone()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        if !self.operations.contains(&operation.key()) {
            return Err(ApiError::Invalid("unnegotiated Files operation".into()));
        }
        let invocation = self.dispatch.admit(&operation.call(), CallOrigin::Local)?;
        invocation.invoke(input).await
    }
}

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
    operations: Vec<OperationSpec>,
    input: ByteBudget,
}

pub(crate) fn client(
    dispatch: Arc<dyn ApiDispatch>,
    description: Arc<ConnectionDescription>,
) -> Result<SessionFilesClient> {
    let operations = [
        FilesOperation::Open,
        FilesOperation::Read,
        FilesOperation::List,
        FilesOperation::Release,
    ]
    .map(FilesOperation::spec)
    .to_vec();
    let input = dispatch
        .admit(&FilesOperation::Open.spec().id, CallOrigin::Local)?
        .input_budget();
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
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        self.input.clone()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        if !self.operations.contains(operation) {
            return Err(ApiError::Invalid("unnegotiated Files operation".into()));
        }
        let invocation = self.dispatch.admit(&operation.id, CallOrigin::Local)?;
        if invocation.spec() != operation {
            return Err(ApiError::Invalid("Files operation contract changed".into()));
        }
        invocation.invoke(input).await
    }
}

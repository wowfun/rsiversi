use crate::wire::{self, Operation};
use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_api_protocol::{
    ApiClient, ApiDispatch, ApiError, ApiOutput, ByteBudget, CallOrigin, ConnectionDescription,
    OperationClass, OperationSpec, Result, RetainedBytes,
};
use std::sync::Arc;

pub(crate) fn operations(available: &[OperationSpec]) -> Result<Vec<OperationSpec>> {
    Operation::ALL
        .into_iter()
        .filter(|operation| !matches!(operation, Operation::Create | Operation::Recent))
        .map(Operation::spec)
        .map(|operation| {
            if available.contains(&operation) {
                Ok(operation)
            } else {
                Err(ApiError::Unavailable)
            }
        })
        .collect()
}
#[derive(Debug)]
enum Backend {
    Client(Arc<dyn ApiClient>),
    Dispatch {
        api: Arc<dyn ApiDispatch>,
        origin: CallOrigin,
    },
}
/// Domain-owned API authority restricted to one Session and its existing handle operations.
#[derive(Debug)]
pub struct SessionTargetClient {
    session: SessionId,
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    input: [ByteBudget; 3],
    backend: Backend,
}
impl SessionTargetClient {
    /// Narrows an existing connection without changing its authentication or receipt semantics.
    pub fn new(api: Arc<dyn ApiClient>, session: SessionId) -> Result<Self> {
        let operations = operations(api.operations())?;
        Ok(Self {
            session,
            operations,
            description: api.description().clone(),
            input: [
                OperationClass::Control,
                OperationClass::Data,
                OperationClass::Subscription,
            ]
            .map(|class| api.input_budget(class)),
            backend: Backend::Client(api),
        })
    }
    /// Uses one explicitly supplied registry generation and trusted non-serialized origin.
    pub fn from_dispatch(
        api: Arc<dyn ApiDispatch>,
        description: ConnectionDescription,
        origin: CallOrigin,
        session: SessionId,
    ) -> Result<Self> {
        let operations = operations(&api.operations())?;
        // Borrow the existing lane pools through ordinary admission; the unused
        // invocation drops immediately without dispatching domain work.
        let input = [
            Operation::MessageStatus,
            Operation::Attach,
            Operation::Observe,
        ]
        .map(|operation| {
            api.admit(&operation.spec().id, origin.clone())
                .map(|invocation| invocation.input_budget())
        });
        let [control, data, subscription] = input;
        Ok(Self {
            session,
            description,
            operations,
            input: [control?, data?, subscription?],
            backend: Backend::Dispatch { api, origin },
        })
    }
    fn validate_target(&self, operation: &OperationSpec, input: &RetainedBytes) -> Result<()> {
        if !self.operations.contains(operation) {
            return Err(ApiError::Unauthorized);
        }
        if input.len() > operation.maximum_request_bytes {
            return Err(ApiError::Invalid(
                "Session request exceeds its operation bound".into(),
            ));
        }
        let session = if operation.id == Operation::Attach.spec().id {
            let request: wire::Attach =
                serde_json::from_slice(input.as_bytes()).map_err(|_| invalid())?;
            request.session_id
        } else {
            // Inspect only the closed target envelope, without allocating a second
            // copy of potentially large Image or message content.
            let request: wire::HandleRequest<serde::de::IgnoredAny> =
                serde_json::from_slice(input.as_bytes()).map_err(|_| invalid())?;
            request.target.validate().map_err(|_| invalid())?;
            request.target.session_id
        };
        if session != self.session {
            return Err(ApiError::Unauthorized);
        }
        Ok(())
    }
}
#[async_trait]
impl ApiClient for SessionTargetClient {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, class: OperationClass) -> ByteBudget {
        self.input[match class {
            OperationClass::Control => 0,
            OperationClass::Data => 1,
            OperationClass::Subscription => 2,
        }]
        .clone()
    }
    async fn call(&self, operation: &OperationSpec, input: RetainedBytes) -> Result<ApiOutput> {
        self.validate_target(operation, &input)?;
        match &self.backend {
            Backend::Client(api) => api.call(operation, input).await,
            Backend::Dispatch { api, origin } => {
                let invocation = api.admit(&operation.id, origin.clone())?;
                if invocation.spec() != operation {
                    return Err(ApiError::Unavailable);
                }
                invocation.invoke(input).await
            }
        }
    }
}
fn invalid() -> ApiError {
    ApiError::Invalid("invalid Session target envelope".into())
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;

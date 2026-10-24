use crate::wire::{self, Operation};
use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_api_protocol::{
    ApiClient, ApiDispatch, ApiError, ApiOutput, ByteBudget, CallOrigin, ConnectionDescription,
    OperationClass, OperationSpec, Result, RetainedBytes,
};
use std::sync::Arc;

pub(crate) fn operations() -> Vec<OperationSpec> {
    Operation::ALL
        .into_iter()
        .filter(|operation| match operation {
            Operation::CasGcPreview
            | Operation::CasGc
            | Operation::CasGcStatus
            | Operation::PurgePreview
            | Operation::Purge
            | Operation::PurgeStatus
            | Operation::Create
            | Operation::Recent
            | Operation::ReadHeader
            | Operation::WorkflowReadiness
            | Operation::ListWorkflows
            | Operation::ReadWorkflow
            | Operation::CancelWorkflow => false,
            Operation::Export
            | Operation::Terminal
            | Operation::TerminalOutput
            | Operation::TerminalInput
            | Operation::Attach
            | Operation::Submit
            | Operation::Commands
            | Operation::ExecuteCommand
            | Operation::CommandStatus
            | Operation::DraftSnapshot
            | Operation::SelectPreset
            | Operation::Image
            | Operation::MutateQueue
            | Operation::QueueMutationStatus
            | Operation::MessageStatus
            | Operation::ReadMessage
            | Operation::Cancel
            | Operation::History
            | Operation::ModelObserve
            | Operation::Observe
            | Operation::Interactions
            | Operation::Projections
            | Operation::Resource
            | Operation::CaptureReference
            | Operation::PreviewReference
            | Operation::ReadReference
            | Operation::ReleaseReference
            | Operation::GoalControl
            | Operation::GoalStatus
            | Operation::GoalObserve
            | Operation::Jobs
            | Operation::PeekJob
            | Operation::Inspect
            | Operation::Metrics
            | Operation::TreeMetrics
            | Operation::Evidence
            | Operation::Questions
            | Operation::AnswerQuestion
            | Operation::Approvals
            | Operation::AnswerApproval => true,
        })
        .map(Operation::spec)
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
    operations: Arc<[rsi_api_protocol::OperationKey]>,
    input: [ByteBudget; 3],
    backend: Backend,
}
impl SessionTargetClient {
    /// Narrows an existing connection without changing its authentication or receipt semantics.
    pub fn new(api: Arc<dyn ApiClient>, session: SessionId) -> Self {
        let operations = operations()
            .iter()
            .map(OperationSpec::key)
            .collect::<Vec<_>>()
            .into();
        Self {
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
        }
    }
    /// Uses one explicitly supplied registry generation and trusted non-serialized origin.
    pub fn from_dispatch(
        api: Arc<dyn ApiDispatch>,
        description: ConnectionDescription,
        origin: CallOrigin,
        session: SessionId,
    ) -> Result<Self> {
        let operations = operations()
            .iter()
            .map(OperationSpec::key)
            .collect::<Vec<_>>()
            .into();
        let input = [
            OperationClass::Control,
            OperationClass::Data,
            OperationClass::Subscription,
        ]
        .map(|class| api.input_budget(class));
        Ok(Self {
            session,
            description,
            operations,
            input,
            backend: Backend::Dispatch { api, origin },
        })
    }
    /// Fixed explicitly granted operation keys, independent of current availability.
    pub fn allowed_keys(&self) -> Arc<[rsi_api_protocol::OperationKey]> {
        self.operations.clone()
    }
    fn validate_target(&self, operation: &OperationSpec, input: &RetainedBytes) -> Result<()> {
        if !self.operations.contains(&operation.key()) {
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
        } else if operation.id == Operation::CaptureReference.spec().id {
            let request: wire::HandleRequest<wire::Attach> =
                serde_json::from_slice(input.as_bytes()).map_err(|_| invalid())?;
            request.target.validate().map_err(|_| invalid())?;
            if request.input.session_id != self.session {
                return Err(ApiError::Unauthorized);
            }
            request.target.session_id
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
    async fn operations(&self) -> Result<rsi_api_protocol::OperationCatalog> {
        let current = match &self.backend {
            Backend::Client(api) => api.operations().await?.operations().to_vec(),
            Backend::Dispatch { api, origin } => api
                .operations()?
                .into_iter()
                .filter(|spec| spec.access.permits(origin))
                .collect(),
        };
        rsi_api_protocol::OperationCatalog::new(
            current
                .into_iter()
                .filter(|spec| self.operations.contains(&spec.key()))
                .collect(),
        )
    }
    async fn operation(&self, key: &rsi_api_protocol::OperationKey) -> Result<OperationSpec> {
        if !self.operations.contains(key) {
            return Err(ApiError::Unauthorized);
        }
        match &self.backend {
            Backend::Client(api) => api.operation(key).await,
            Backend::Dispatch { api, origin } => api.operation(key, origin),
        }
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
                let invocation = api.admit(&operation.call(), origin.clone())?;
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

#[cfg(test)]
mod workflow_tests {
    use super::*;
    #[test]
    fn scoped_plugins_do_not_implicitly_receive_workflow_operations() {
        let narrowed = operations();
        for op in [
            Operation::WorkflowReadiness,
            Operation::ListWorkflows,
            Operation::ReadWorkflow,
            Operation::CancelWorkflow,
        ] {
            assert!(!narrowed.contains(&op.spec()));
        }
        assert!(narrowed.contains(&Operation::Submit.spec()));
    }
}

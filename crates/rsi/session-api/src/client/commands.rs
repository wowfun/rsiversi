use super::{ApiError, Handle, Operation, SessionError, malformed};
use rsi_agent_session_protocol::{
    CommandRevision, DomainRequestId, SessionCommandInvocation, SessionCommandReceipt,
};

impl Handle {
    pub(super) async fn control_checked_goal(
        &self,
        request: rsi_goal::GoalControl,
    ) -> rsi_session_protocol::Result<rsi_goal::GoalControlReceipt> {
        let invocation = request
            .invocation()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let digest = invocation
            .digest()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let successor = match request.expected_revision {
            CommandRevision::Draft { revision } => revision
                .checked_add(1)
                .map(|revision| CommandRevision::Draft { revision }),
            CommandRevision::Durable { control_seq } => control_seq
                .checked_add(1)
                .map(|control_seq| CommandRevision::Durable { control_seq }),
        }
        .ok_or_else(|| SessionError::Invalid("Goal command revision cannot advance".into()))?;
        let unknown = || SessionError::CommandOutcomeUnknown {
            request_id: request.request_id.clone(),
        };
        let result: rsi_session_protocol::Result<rsi_goal::GoalControlReceipt> =
            self.call(Operation::GoalControl, &request).await;
        match result {
            Ok(receipt)
                if receipt.command.request_id() == &request.request_id
                    && receipt.command.command() == &invocation.command
                    && receipt.command.invocation_sha256() == digest
                    && receipt.command.revision() == successor
                    && receipt.live.validate().is_ok() =>
            {
                Ok(receipt)
            }
            Ok(_) | Err(SessionError::Api(ApiError::OutcomeUnknown)) => Err(unknown()),
            Err(
                error @ (SessionError::CommandConflict { .. }
                | SessionError::CommandOutcomeUnknown { .. }),
            ) => {
                let (SessionError::CommandConflict { request_id }
                | SessionError::CommandOutcomeUnknown { request_id }) = &error
                else {
                    unreachable!()
                };
                if request_id == &request.request_id {
                    Err(error)
                } else {
                    Err(unknown())
                }
            }
            Err(error @ SessionError::CommandRevisionConflict { .. }) => {
                let SessionError::CommandRevisionConflict { expected, .. } = &error else {
                    unreachable!()
                };
                if expected == &request.expected_revision {
                    Err(error)
                } else {
                    Err(unknown())
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn execute_checked_command(
        &self,
        invocation: SessionCommandInvocation,
    ) -> rsi_session_protocol::Result<SessionCommandReceipt> {
        let digest = invocation
            .digest()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let successor = match invocation.expected_revision {
            CommandRevision::Draft { revision } => revision
                .checked_add(1)
                .map(|revision| CommandRevision::Draft { revision }),
            CommandRevision::Durable { control_seq } => control_seq
                .checked_add(1)
                .map(|control_seq| CommandRevision::Durable { control_seq }),
        }
        .ok_or_else(|| SessionError::Invalid("command revision cannot advance".into()))?;
        let unknown = || SessionError::CommandOutcomeUnknown {
            request_id: invocation.request_id.clone(),
        };
        let result: rsi_session_protocol::Result<SessionCommandReceipt> =
            self.call(Operation::ExecuteCommand, &invocation).await;
        match result {
            Ok(receipt)
                if receipt.request_id() == &invocation.request_id
                    && receipt.command() == &invocation.command
                    && receipt.invocation_sha256() == digest
                    && receipt.revision() == successor =>
            {
                Ok(receipt)
            }
            Ok(_) | Err(SessionError::Api(ApiError::OutcomeUnknown)) => Err(unknown()),
            Err(
                error @ (SessionError::CommandConflict { .. }
                | SessionError::CommandOutcomeUnknown { .. }),
            ) => {
                let (SessionError::CommandConflict { request_id }
                | SessionError::CommandOutcomeUnknown { request_id }) = &error
                else {
                    unreachable!()
                };
                if request_id == &invocation.request_id {
                    Err(error)
                } else {
                    Err(unknown())
                }
            }
            Err(error @ SessionError::CommandRevisionConflict { .. }) => {
                let SessionError::CommandRevisionConflict { expected, .. } = &error else {
                    unreachable!()
                };
                if expected == &invocation.expected_revision {
                    Err(error)
                } else {
                    Err(unknown())
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) async fn checked_command_status(
        &self,
        request_id: &DomainRequestId,
    ) -> rsi_session_protocol::Result<Option<SessionCommandReceipt>> {
        let result: rsi_session_protocol::Result<Option<SessionCommandReceipt>> =
            self.call(Operation::CommandStatus, request_id).await;
        match result {
            Ok(Some(receipt)) if receipt.request_id() != request_id => {
                Err(malformed(Operation::CommandStatus))
            }
            Err(SessionError::CommandConflict { request_id: actual }) if &actual != request_id => {
                Err(malformed(Operation::CommandStatus))
            }
            other => other,
        }
    }
}

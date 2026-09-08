use crate::wire::{self, Failure, HandleReply, HandleRequest, Operation, Target};
use async_trait::async_trait;
use rsi_agent_session_protocol::{AgentMessage, MessageId, SessionHeader, SessionId};
use rsi_agent_store_protocol::{
    StoreBackwardFactPage, StoreRecentSession, StoreRecentSessionCursor, StoreRecentSessionPage,
    StoreSessionInspection,
};
use rsi_agent_turn_protocol::{
    CancelResult, CancelTarget, MessageReceipt, ObservationCursor, ObservationRetention,
    SessionObservationStream,
};
use rsi_api_protocol::{ApiClient, ApiError, OperationEffect, call_json};
use rsi_approval_protocol::{ApprovalDecision, ApprovalRequest};
use rsi_session_protocol::{
    CreateSession, InteractionRetention, InteractionStream, RecentSessionCursor, RecentSessionPage,
    SessionError, SessionHandle, SessionHistoryPage, SessionService, SessionSummary,
    SubmitDirectImage, SubmitInput, TurnReceipt, validate_session_input,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeSet, sync::Arc};

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

mod commands;
mod draft;

#[derive(Debug)]
pub(super) struct State {
    pub api: Arc<dyn ApiClient>,
    pub observations: ObservationRetention,
    pub interactions: InteractionRetention,
    pub projections: rsi_session_protocol::ProjectionRetention,
}
/// Shared Session application proxy over one negotiated API generation.
#[derive(Clone, Debug)]
pub struct SessionClient {
    state: Arc<State>,
}
impl SessionClient {
    /// Requires every exact Session operation before publishing the application capability.
    pub fn new(api: Arc<dyn ApiClient>) -> rsi_api_protocol::Result<Self> {
        if Operation::ALL
            .iter()
            .any(|operation| !api.operations().contains(&operation.spec()))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self {
            state: Arc::new(State {
                api,
                observations: ObservationRetention::default(),
                interactions: InteractionRetention::default(),
                projections: rsi_session_protocol::ProjectionRetention::default(),
            }),
        })
    }
    fn handle(
        &self,
        header: SessionHeader,
        draft_revision: Option<u64>,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        let target = Target {
            session_id: header.session_id().clone(),
            header_key: header
                .fingerprint()
                .map_err(|_| malformed(Operation::Attach))?,
        };
        Ok(Arc::new(Handle {
            state: self.state.clone(),
            session_id: header.session_id().clone(),
            binding: Arc::new(std::sync::RwLock::new(Binding {
                header,
                target,
                draft_revision,
            })),
        }))
    }
}
pub(super) fn malformed(operation: Operation) -> SessionError {
    SessionError::Api(if operation.spec().effect == OperationEffect::Mutation {
        ApiError::OutcomeUnknown
    } else {
        ApiError::Invalid("invalid remote Session response".into())
    })
}
pub(super) fn failure(
    operation: Operation,
    failure: Failure,
) -> rsi_session_protocol::Result<SessionError> {
    let valid = match &failure {
        Failure::Invalid { message } => message.len() <= 4096,
        Failure::DraftConflict { .. } => operation == Operation::Create,
        Failure::Conflict { .. } => operation == Operation::Image,
        Failure::MessageConflict { .. } | Failure::MessageOutcomeUnknown { .. } => {
            operation == Operation::Submit
        }
        Failure::CommandConflict { .. } => matches!(
            operation,
            Operation::ExecuteCommand | Operation::CommandStatus
        ),
        Failure::CommandOutcomeUnknown { .. } => operation == Operation::ExecuteCommand,
        Failure::CommandRevisionConflict { expected, actual } => {
            matches!(
                operation,
                Operation::ExecuteCommand | Operation::SelectPreset
            ) && expected != actual
        }
        _ => true,
    };
    if valid {
        Ok(failure.into_error())
    } else {
        Err(malformed(operation))
    }
}
impl State {
    pub async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: Operation,
        input: &I,
    ) -> rsi_session_protocol::Result<O> {
        match call_json::<_, O, Failure>(self.api.as_ref(), &operation.spec(), input)
            .await
            .map_err(SessionError::Api)?
        {
            Ok(value) => Ok(value),
            Err(error) => Err(failure(operation, error)?),
        }
    }
}
#[async_trait]
impl SessionService for SessionClient {
    async fn create(
        &self,
        request: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        let result: rsi_session_protocol::Result<wire::Created> =
            self.state.call(Operation::Create, &request).await;
        if matches!(&result, Err(SessionError::DraftConflict { session }) if session != request.session_id.as_str())
        {
            return Err(malformed(Operation::Create));
        }
        let created = result?;
        if created.creation != request {
            return Err(malformed(Operation::Create));
        }
        let header = created.draft.header;
        let workspace = rsi_workspace_protocol::WorkspaceRecord {
            id: request.workspace_id,
            path: header.canonical_cwd().into(),
        };
        if header.session_id() != &request.session_id
            || workspace.validate().is_err()
            || header.workspace_trust() != request.workspace_trust
            || (created.draft.revision == 0
                && request
                    .agent_preset_id
                    .as_ref()
                    .is_some_and(|preset| header.agent_preset_id() != preset))
        {
            return Err(malformed(Operation::Create));
        }
        self.handle(header, Some(created.draft.revision))
    }
    async fn attach(
        &self,
        session_id: &SessionId,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        let header: SessionHeader = self
            .state
            .call(
                Operation::Attach,
                &wire::Attach {
                    session_id: session_id.clone(),
                },
            )
            .await?;
        if header.session_id() != session_id {
            return Err(malformed(Operation::Attach));
        }
        self.handle(header, None)
    }
    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> rsi_session_protocol::Result<RecentSessionPage> {
        rsi_agent_store_protocol::validate_session_read_limit(limit)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let page: RecentSessionPage = self
            .state
            .call(
                Operation::Recent,
                &wire::Recent {
                    after: after.cloned(),
                    limit,
                },
            )
            .await?;
        if page.sessions.len() > limit.min(wire::RECENT_READ_LIMIT) {
            return Err(malformed(Operation::Recent));
        }
        let checked = StoreRecentSessionPage {
            after: after.map(|cursor| StoreRecentSessionCursor {
                created_at_ms: cursor.created_at_ms,
                session_id: cursor.session_id.clone(),
            }),
            sessions: page
                .sessions
                .into_iter()
                .map(|row| StoreRecentSession { header: row.header })
                .collect(),
            has_more: page.has_more,
        };
        checked
            .validate()
            .map_err(|_| malformed(Operation::Recent))?;
        Ok(RecentSessionPage {
            sessions: checked
                .sessions
                .into_iter()
                .map(|row| SessionSummary { header: row.header })
                .collect(),
            has_more: checked.has_more,
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct Handle {
    pub state: Arc<State>,
    pub session_id: SessionId,
    binding: Arc<std::sync::RwLock<Binding>>,
}
#[derive(Clone, Debug)]
struct Binding {
    header: SessionHeader,
    target: Target,
    draft_revision: Option<u64>,
}
impl Handle {
    fn binding(&self) -> Binding {
        self.binding
            .read()
            .expect("Session binding poisoned")
            .clone()
    }
    pub(super) fn target(&self) -> Target {
        self.binding().target
    }
    pub(super) fn frozen(&self) -> Self {
        Self {
            state: self.state.clone(),
            session_id: self.session_id.clone(),
            binding: Arc::new(std::sync::RwLock::new(self.binding())),
        }
    }
    pub async fn call<I: Serialize + Sync, O: DeserializeOwned>(
        &self,
        operation: Operation,
        input: &I,
    ) -> rsi_session_protocol::Result<O> {
        let target = self.target();
        let reply: HandleReply<O> = self
            .state
            .call(
                operation,
                &HandleRequest {
                    target: target.clone(),
                    input,
                },
            )
            .await?;
        if reply.target != target {
            return Err(malformed(operation));
        }
        Ok(reply.body)
    }
    pub async fn verify_owners(
        &self,
        requests: &[ApprovalRequest],
        verified: &mut BTreeSet<String>,
    ) -> rsi_session_protocol::Result<()> {
        if requests
            .iter()
            .any(|request| !verified.contains(request.subject.session_id()))
        {
            let tree = self.inspect().await?.tree;
            verified.insert(tree.session.session_id.to_string());
            verified.extend(
                tree.descendants
                    .into_iter()
                    .map(|entry| entry.status.session_id.to_string()),
            );
            if verified.len() > rsi_agent_session_protocol::MAXIMUM_DURABLE_AGENT_TREE_NODES
                || requests
                    .iter()
                    .any(|request| !verified.contains(request.subject.session_id()))
            {
                return Err(malformed(Operation::Approvals));
            }
        }
        Ok(())
    }
}
#[async_trait]
impl SessionHandle for Handle {
    async fn observe_projections(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::ProjectionStream> {
        crate::client_stream::projections(self).await
    }
    async fn draft_snapshot(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        self.checked_draft_snapshot().await
    }
    async fn select_preset(
        &self,
        request: rsi_session_protocol::SelectDraftPreset,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionDraftView> {
        self.select_checked_preset(request).await
    }

    async fn commands(
        &self,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandsView> {
        self.call(Operation::Commands, &()).await
    }
    async fn execute_command(
        &self,
        invocation: rsi_agent_session_protocol::SessionCommandInvocation,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionCommandReceipt> {
        self.execute_checked_command(invocation).await
    }
    async fn command_status(
        &self,
        request_id: &rsi_agent_session_protocol::DomainRequestId,
    ) -> rsi_session_protocol::Result<Option<rsi_agent_session_protocol::SessionCommandReceipt>>
    {
        self.checked_command_status(request_id).await
    }
    async fn header(&self) -> rsi_session_protocol::Result<SessionHeader> {
        Ok(self.binding().header)
    }
    async fn read_message(
        &self,
        message_id: &MessageId,
        accepted_control_seq: u64,
    ) -> rsi_session_protocol::Result<AgentMessage> {
        if accepted_control_seq == 0 {
            return Err(SessionError::Invalid(
                "acceptance cursor must be positive".into(),
            ));
        }
        let reply: wire::MessageReadReply = self
            .call(
                Operation::ReadMessage,
                &wire::MessageRead {
                    message_id: message_id.clone(),
                    accepted_control_seq,
                },
            )
            .await?;
        if reply.accepted_control_seq != accepted_control_seq
            || reply.message.message_id != *message_id
            || reply.message.validate().is_err()
        {
            return Err(malformed(Operation::ReadMessage));
        }
        Ok(reply.message)
    }
    async fn submit(&self, request: SubmitInput) -> rsi_session_protocol::Result<MessageReceipt> {
        validate_session_input(&request.content)?;
        let result = self.call(Operation::Submit, &request).await;
        match result {
            Ok(receipt) => self.receipt(receipt, &request.message_id, Operation::Submit),
            Err(SessionError::Api(ApiError::OutcomeUnknown)) => {
                Err(SessionError::MessageOutcomeUnknown {
                    session: self.session_id.to_string(),
                    message: request.message_id.to_string(),
                })
            }
            Err(
                error @ (SessionError::MessageConflict { .. }
                | SessionError::MessageOutcomeUnknown { .. }),
            ) => {
                let (SessionError::MessageConflict { session, message }
                | SessionError::MessageOutcomeUnknown { session, message }) = &error
                else {
                    unreachable!()
                };
                if session != self.session_id.as_str() || message != request.message_id.as_str() {
                    return Err(SessionError::MessageOutcomeUnknown {
                        session: self.session_id.to_string(),
                        message: request.message_id.to_string(),
                    });
                }
                Err(error)
            }
            Err(error) => Err(error),
        }
    }
    async fn message_status(
        &self,
        message_id: &MessageId,
    ) -> rsi_session_protocol::Result<MessageReceipt> {
        self.receipt(
            self.call(Operation::MessageStatus, message_id).await?,
            message_id,
            Operation::MessageStatus,
        )
    }
    async fn generate_image(
        &self,
        request: SubmitDirectImage,
    ) -> rsi_session_protocol::Result<TurnReceipt> {
        let result: rsi_session_protocol::Result<TurnReceipt> =
            self.call(Operation::Image, &request).await;
        if matches!(&result, Err(SessionError::Conflict { session, turn }) if session != self.session_id.as_str() || turn != request.turn_id.as_str())
        {
            return Err(malformed(Operation::Image));
        }
        let receipt = result?;
        if receipt.session_id != self.session_id
            || receipt.turn_id != request.turn_id
            || receipt.accepted_seq == 0
        {
            return Err(malformed(Operation::Image));
        }
        Ok(receipt)
    }
    async fn cancel(
        &self,
        target: CancelTarget,
        reason: Option<String>,
    ) -> rsi_session_protocol::Result<CancelResult> {
        let result: CancelResult = self
            .call(Operation::Cancel, &wire::Cancel { target, reason })
            .await?;
        if result.accepted && result.already_terminal {
            return Err(malformed(Operation::Cancel));
        }
        Ok(result)
    }
    async fn history_before(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> rsi_session_protocol::Result<SessionHistoryPage> {
        rsi_agent_store_protocol::validate_read_limit(limit)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let page: SessionHistoryPage = self
            .call(Operation::History, &wire::History { before, limit })
            .await?;
        let maximum_before = page
            .durable_seq
            .checked_add(1)
            .ok_or_else(|| malformed(Operation::History))?;
        let expected = before
            .filter(|cursor| *cursor != 0)
            .unwrap_or(maximum_before);
        if page.facts.len() > limit
            || (page.before_seq != expected
                && !(page.durable_seq == 0 && page.before_seq == 1 && page.facts.is_empty()))
        {
            return Err(malformed(Operation::History));
        }
        let checked = StoreBackwardFactPage {
            before_seq: page.before_seq,
            facts: page.facts,
            durable_seq: page.durable_seq,
            has_more: page.has_more,
        };
        checked
            .validate()
            .map_err(|_| malformed(Operation::History))?;
        Ok(SessionHistoryPage {
            before_seq: checked.before_seq,
            facts: checked.facts,
            durable_seq: checked.durable_seq,
            has_more: checked.has_more,
        })
    }
    async fn observe(
        &self,
        cursor: ObservationCursor,
    ) -> rsi_session_protocol::Result<SessionObservationStream> {
        crate::client_stream::observe(self, cursor).await
    }
    async fn observe_interactions(&self) -> rsi_session_protocol::Result<InteractionStream> {
        crate::client_stream::interactions(self).await
    }
    async fn inspect(&self) -> rsi_session_protocol::Result<StoreSessionInspection> {
        let frozen = self.frozen();
        let inspection: StoreSessionInspection = frozen.call(Operation::Inspect, &()).await?;
        inspection
            .validate()
            .map_err(|_| malformed(Operation::Inspect))?;
        if inspection.header != frozen.binding().header {
            return Err(malformed(Operation::Inspect));
        }
        Ok(inspection)
    }
    async fn pending_questions(
        &self,
    ) -> rsi_session_protocol::Result<Vec<rsi_user_questions_protocol::QuestionRequest>> {
        let requests: Vec<rsi_user_questions_protocol::QuestionRequest> =
            self.call(Operation::Questions, &()).await?;
        validate_questions(&requests, &self.session_id)?;
        Ok(requests)
    }
    async fn answer_question(
        &self,
        id: &str,
        answer: rsi_user_questions_protocol::QuestionAnswer,
    ) -> rsi_session_protocol::Result<bool> {
        answer
            .validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        rsi_user_questions_protocol::validate_identity(id)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        self.call(
            Operation::AnswerQuestion,
            &wire::QuestionAnswer {
                id: id.into(),
                answer,
            },
        )
        .await
    }
    async fn pending_approvals(&self) -> rsi_session_protocol::Result<Vec<ApprovalRequest>> {
        let requests: Vec<ApprovalRequest> = self.call(Operation::Approvals, &()).await?;
        validate_approvals(&requests)?;
        self.verify_owners(
            &requests,
            &mut BTreeSet::from([self.session_id.to_string()]),
        )
        .await?;
        Ok(requests)
    }
    async fn answer_approval(
        &self,
        owner: &SessionId,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> rsi_session_protocol::Result<bool> {
        self.call(
            Operation::AnswerApproval,
            &wire::ApprovalAnswer {
                owner: owner.clone(),
                id: approval_id.into(),
                decision,
            },
        )
        .await
    }
}
impl Handle {
    fn receipt(
        &self,
        receipt: MessageReceipt,
        message: &MessageId,
        operation: Operation,
    ) -> rsi_session_protocol::Result<MessageReceipt> {
        if receipt.session_id != self.session_id
            || receipt.message_id != *message
            || receipt.validate().is_err()
        {
            return Err(if operation == Operation::Submit {
                SessionError::MessageOutcomeUnknown {
                    session: self.session_id.to_string(),
                    message: message.to_string(),
                }
            } else {
                malformed(operation)
            });
        }
        Ok(receipt)
    }
}
pub(super) fn validate_questions(
    requests: &[rsi_user_questions_protocol::QuestionRequest],
    session: &SessionId,
) -> rsi_session_protocol::Result<()> {
    let mut ids = BTreeSet::new();
    if requests.len() > 256 {
        return Err(malformed(Operation::Questions));
    }
    for request in requests {
        if request.validate().is_err()
            || request.session_id != session.as_str()
            || !ids.insert(&request.id)
        {
            return Err(malformed(Operation::Questions));
        }
    }
    Ok(())
}
pub(super) fn validate_approvals(requests: &[ApprovalRequest]) -> rsi_session_protocol::Result<()> {
    if requests.len() > 1024 {
        return Err(malformed(Operation::Approvals));
    }
    let mut ids = BTreeSet::new();
    let mut bytes = 0usize;
    for request in requests {
        request
            .validate()
            .map_err(|_| malformed(Operation::Approvals))?;
        if !ids.insert((request.subject.session_id(), &request.id)) {
            return Err(malformed(Operation::Approvals));
        }
        bytes = bytes
            .checked_add(
                request
                    .encoded_len()
                    .map_err(|_| malformed(Operation::Approvals))?,
            )
            .filter(|bytes| *bytes <= 16 * 1024 * 1024)
            .ok_or_else(|| malformed(Operation::Approvals))?;
    }
    Ok(())
}

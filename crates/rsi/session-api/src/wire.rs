use rsi_agent_session_protocol::{CommandRevision, DomainRequestId, MessageId, SessionId, TurnId};
use rsi_agent_turn_protocol::{CancelTarget, ObservationCursor};
use rsi_api_protocol::{
    ApiError, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use rsi_session_protocol::{RecentSessionCursor, SessionError};
use serde::{Deserialize, Serialize};

pub(crate) const HEADER_REPLY: usize = rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES;
pub(crate) const LARGE_REPLY: usize = rsi_api_protocol::MAXIMUM_API_BYTES;
pub(crate) const OBSERVATION_REPLY: usize =
    rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 64 * 1024;
pub(crate) const INTERACTION_REPLY: usize = 32 * 1024 * 1024 + 64 * 1024;
pub(crate) const PROJECTION_REPLY: usize =
    rsi_agent_session_protocol::MAXIMUM_SESSION_PROJECTION_BYTES + 64 * 1024;
pub(crate) const RECENT_READ_LIMIT: usize = (LARGE_REPLY - 64 * 1024) / HEADER_REPLY;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Operation {
    Create,
    Attach,
    Recent,
    Submit,
    Commands,
    ExecuteCommand,
    CommandStatus,
    DraftSnapshot,
    SelectPreset,
    Image,
    MessageStatus,
    ReadMessage,
    Cancel,
    History,
    Observe,
    Interactions,
    Projections,
    Inspect,
    Questions,
    AnswerQuestion,
    Approvals,
    AnswerApproval,
}
impl Operation {
    pub const ALL: [Self; 22] = [
        Self::Create,
        Self::Attach,
        Self::Recent,
        Self::Submit,
        Self::Commands,
        Self::ExecuteCommand,
        Self::CommandStatus,
        Self::DraftSnapshot,
        Self::SelectPreset,
        Self::Image,
        Self::MessageStatus,
        Self::ReadMessage,
        Self::Cancel,
        Self::History,
        Self::Observe,
        Self::Interactions,
        Self::Projections,
        Self::Inspect,
        Self::Questions,
        Self::AnswerQuestion,
        Self::Approvals,
        Self::AnswerApproval,
    ];
    pub fn spec(self) -> OperationSpec {
        use OperationClass::{Control, Data, Subscription};
        use OperationEffect::{Mutation, Read};
        let (name, class, effect, input, output) = match self {
            Self::Create => ("create", Data, Mutation, 8192, HEADER_REPLY + 16 * 1024),
            Self::Attach => ("attach", Data, Read, 4096, HEADER_REPLY),
            Self::Recent => ("recent", Data, Read, 4096, LARGE_REPLY),
            Self::Submit => ("submit", Data, Mutation, 8 * 1024 * 1024, 16 * 1024),
            Self::Commands => ("commands", Data, Read, 8192, 512 * 1024),
            Self::ExecuteCommand => ("execute-command", Data, Mutation, 32 * 1024, 8192),
            Self::CommandStatus => ("command-status", Data, Read, 8192, 8192),
            Self::DraftSnapshot => ("draft-snapshot", Data, Read, 8192, HEADER_REPLY + 8192),
            Self::SelectPreset => ("select-preset", Data, Mutation, 8192, HEADER_REPLY + 8192),
            Self::Image => ("image", Data, Mutation, LARGE_REPLY, 16 * 1024),
            Self::MessageStatus => ("message-status", Control, Read, 8192, 16 * 1024),
            Self::ReadMessage => ("read-message", Data, Read, 8192, OBSERVATION_REPLY),
            Self::Cancel => ("cancel", Control, Mutation, 128 * 1024, 8192),
            Self::History => ("history", Data, Read, 8192, LARGE_REPLY),
            Self::Observe => ("observe", Subscription, Read, 8192, OBSERVATION_REPLY),
            Self::Interactions => ("interactions", Subscription, Read, 8192, INTERACTION_REPLY),
            Self::Projections => ("projections", Subscription, Read, 8192, PROJECTION_REPLY),
            Self::Inspect => ("inspect", Data, Read, 8192, LARGE_REPLY),
            Self::Questions => ("questions", Data, Read, 8192, INTERACTION_REPLY),
            Self::AnswerQuestion => ("answer-question", Control, Mutation, 128 * 1024, 8192),
            Self::Approvals => ("approvals", Data, Read, 8192, INTERACTION_REPLY),
            Self::AnswerApproval => ("answer-approval", Control, Mutation, 128 * 1024, 8192),
        };
        OperationSpec {
            id: OperationId::new("session", name, if self == Self::Create { 2 } else { 1 })
                .expect("constant operation"),
            class,
            effect,
            access: rsi_api_protocol::OperationAccess::Authenticated,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: input,
            maximum_response_bytes: output,
        }
    }
}

pub(crate) type Target = rsi_session_protocol::SessionTarget;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandleRequest<T> {
    pub target: Target,
    pub input: T,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandleReply<T> {
    pub target: Target,
    pub body: T,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Attach {
    pub session_id: SessionId,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Created {
    pub creation: rsi_session_protocol::CreateSession,
    pub draft: rsi_session_protocol::SessionDraftView,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Recent {
    pub after: Option<RecentSessionCursor>,
    pub limit: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct History {
    pub before: Option<u64>,
    pub limit: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageRead {
    pub message_id: MessageId,
    pub accepted_control_seq: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageReadReply {
    pub accepted_control_seq: u64,
    pub message: rsi_agent_session_protocol::AgentMessage,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cancel {
    pub target: CancelTarget,
    pub reason: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QuestionAnswer {
    pub id: String,
    pub answer: rsi_user_questions_protocol::QuestionAnswer,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalAnswer {
    pub owner: SessionId,
    pub id: String,
    pub decision: rsi_approval_protocol::ApprovalDecision,
}

pub(crate) type Observe = HandleRequest<ObservationCursor>;

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Observation<C, F> {
    Control { record: C, durable_control_seq: u64 },
    Fact { fact: F, durable_fact_seq: u64 },
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Failure {
    Invalid {
        message: String,
    },
    NotFound {},
    DraftConflict {
        session: SessionId,
    },
    Conflict {
        session: SessionId,
        turn: TurnId,
    },
    MessageConflict {
        session: SessionId,
        message: MessageId,
    },
    MessageOutcomeUnknown {
        session: SessionId,
        message: MessageId,
    },
    CommandConflict {
        request_id: DomainRequestId,
    },
    CommandRevisionConflict {
        expected: CommandRevision,
        actual: CommandRevision,
    },
    CommandOutcomeUnknown {
        request_id: DomainRequestId,
    },
    Capacity {},
    ShuttingDown {},
}
pub(crate) fn domain<T>(
    result: rsi_session_protocol::Result<T>,
) -> rsi_api_protocol::Result<Result<T, Failure>> {
    result.map(Ok).or_else(|error| {
        let invalid = |_| ApiError::Backend("invalid Session error identity".into());
        Ok(Err(match error {
            SessionError::Api(error) => return Err(error),
            SessionError::Backend(_) => {
                return Err(ApiError::Backend("Session backend failed".into()));
            }
            SessionError::Invalid(mut message) => {
                let mut end = message.len().min(4096);
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
                Failure::Invalid { message }
            }
            SessionError::NotFound(_) => Failure::NotFound {},
            SessionError::DraftConflict { session } => Failure::DraftConflict {
                session: SessionId::new(session).map_err(invalid)?,
            },
            SessionError::Conflict { session, turn } => Failure::Conflict {
                session: SessionId::new(session).map_err(invalid)?,
                turn: TurnId::new(turn).map_err(invalid)?,
            },
            SessionError::MessageConflict { session, message } => Failure::MessageConflict {
                session: SessionId::new(session).map_err(invalid)?,
                message: MessageId::new(message).map_err(invalid)?,
            },
            SessionError::MessageOutcomeUnknown { session, message } => {
                Failure::MessageOutcomeUnknown {
                    session: SessionId::new(session).map_err(invalid)?,
                    message: MessageId::new(message).map_err(invalid)?,
                }
            }
            SessionError::Capacity => Failure::Capacity {},
            SessionError::CommandConflict { request_id } => Failure::CommandConflict { request_id },
            SessionError::CommandRevisionConflict { expected, actual } => {
                Failure::CommandRevisionConflict { expected, actual }
            }
            SessionError::CommandOutcomeUnknown { request_id } => {
                Failure::CommandOutcomeUnknown { request_id }
            }
            SessionError::ShuttingDown => Failure::ShuttingDown {},
        }))
    })
}
impl Failure {
    pub fn into_error(self) -> SessionError {
        match self {
            Self::Invalid { message } => SessionError::Invalid(message),
            Self::NotFound {} => SessionError::NotFound("remote Session object".into()),
            Self::DraftConflict { session } => SessionError::DraftConflict {
                session: session.to_string(),
            },
            Self::Conflict { session, turn } => SessionError::Conflict {
                session: session.to_string(),
                turn: turn.to_string(),
            },
            Self::MessageConflict { session, message } => SessionError::MessageConflict {
                session: session.to_string(),
                message: message.to_string(),
            },
            Self::MessageOutcomeUnknown { session, message } => {
                SessionError::MessageOutcomeUnknown {
                    session: session.to_string(),
                    message: message.to_string(),
                }
            }
            Self::Capacity {} => SessionError::Capacity,
            Self::CommandConflict { request_id } => SessionError::CommandConflict { request_id },
            Self::CommandRevisionConflict { expected, actual } => {
                SessionError::CommandRevisionConflict { expected, actual }
            }
            Self::CommandOutcomeUnknown { request_id } => {
                SessionError::CommandOutcomeUnknown { request_id }
            }
            Self::ShuttingDown {} => SessionError::ShuttingDown,
        }
    }
}

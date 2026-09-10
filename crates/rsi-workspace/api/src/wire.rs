use rsi_api_protocol::{
    ApiError, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use rsi_workspace_protocol::{WorkspaceCursor, WorkspaceError, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Get,
    List,
    Register,
    Status,
    Delete,
}
impl Operation {
    pub const ALL: [Self; 5] = [
        Self::Get,
        Self::List,
        Self::Register,
        Self::Status,
        Self::Delete,
    ];
    pub fn spec(self) -> OperationSpec {
        let (name, effect) = match self {
            Self::Get => ("get", OperationEffect::Read),
            Self::List => ("list", OperationEffect::Read),
            Self::Register => ("register", OperationEffect::Mutation),
            Self::Status => ("status", OperationEffect::Read),
            Self::Delete => ("delete", OperationEffect::Mutation),
        };
        OperationSpec {
            id: OperationId::new("workspace", name, 1).expect("constant operation"),
            class: OperationClass::Data,
            effect,
            access: rsi_api_protocol::OperationAccess::Authenticated,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: if matches!(self, Self::Register) {
                128 * 1024
            } else {
                128
            },
            maximum_response_bytes: if matches!(self, Self::List) {
                32 * 1024 * 1024
            } else {
                128 * 1024
            },
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdRequest {
    pub id: WorkspaceId,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListRequest {
    pub after: Option<WorkspaceCursor>,
    pub limit: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterRequest {
    pub path: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Failure {
    Capacity,
    ShuttingDown,
    InvalidInput,
    Unknown { id: WorkspaceId },
    Corrupt,
}
pub(crate) fn result<T>(
    result: rsi_workspace_protocol::Result<T>,
) -> rsi_api_protocol::Result<std::result::Result<T, Failure>> {
    Ok(match result {
        Ok(value) => Ok(value),
        Err(error) => Err(match error {
            WorkspaceError::Capacity => Failure::Capacity,
            WorkspaceError::ShuttingDown => Failure::ShuttingDown,
            WorkspaceError::InvalidInput(_) => Failure::InvalidInput,
            WorkspaceError::Unknown(id) => Failure::Unknown { id },
            WorkspaceError::Corrupt(_) => Failure::Corrupt,
            WorkspaceError::Storage(_) => {
                return Err(ApiError::Backend(
                    "workspace storage or commit task failed".into(),
                ));
            }
            WorkspaceError::Api(error) => return Err(error),
        }),
    })
}
impl From<Failure> for WorkspaceError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Capacity => Self::Capacity,
            Failure::ShuttingDown => Self::ShuttingDown,
            Failure::InvalidInput => Self::InvalidInput("remote workspace rejected input".into()),
            Failure::Unknown { id } => Self::Unknown(id),
            Failure::Corrupt => Self::Corrupt("remote workspace state is invalid".into()),
        }
    }
}
pub(crate) fn invalid_record(_: WorkspaceError) -> WorkspaceError {
    WorkspaceError::Api(ApiError::OutcomeUnknown)
}

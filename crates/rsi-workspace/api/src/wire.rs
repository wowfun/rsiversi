use rsi_api_protocol::{
    ApiError, OperationClass, OperationEffect, OperationId, OperationSpec, RequestEncoding,
};
use rsi_workspace_protocol::{WorkspaceCursor, WorkspaceError, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug)]
/// Owner-defined operations for explicit capability discovery.
pub enum Operation {
    /// Get operation.
    Get,
    /// List operation.
    List,
    /// Workspace ordering seed.
    OrderSeed,
    /// Register operation.
    Register,
    /// Replace the display name.
    Rename,
    /// Status operation.
    Status,
    /// Delete operation.
    Delete,
}
impl Operation {
    /// All operation identities owned by this protocol.
    pub const ALL: [Self; 7] = [
        Self::Get,
        Self::List,
        Self::OrderSeed,
        Self::Register,
        Self::Rename,
        Self::Status,
        Self::Delete,
    ];
    /// Local request and response policy for this operation.
    ///
    /// # Panics
    /// Panics if a built-in operation identity is invalid.
    pub fn spec(self) -> OperationSpec {
        let (name, effect) = match self {
            Self::Get => ("get", OperationEffect::Read),
            Self::List => ("list", OperationEffect::Read),
            Self::OrderSeed => ("order_seed", OperationEffect::Read),
            Self::Register => ("register", OperationEffect::Mutation),
            Self::Rename => ("rename", OperationEffect::Mutation),
            Self::Status => ("status", OperationEffect::Read),
            Self::Delete => ("delete", OperationEffect::Mutation),
        };
        OperationSpec {
            protocol: rsi_api_protocol::wire_contract!(),
            id: OperationId::new("workspace", name, 4).expect("constant operation"),
            class: OperationClass::Data,
            effect,
            access: rsi_api_protocol::OperationAccess::Authenticated,
            encoding: RequestEncoding::Json,
            maximum_request_bytes: if matches!(self, Self::Register | Self::Rename) {
                128 * 1024
            } else {
                128
            },
            maximum_response_bytes: if matches!(self, Self::List) {
                32 * 1024 * 1024
            } else if matches!(self, Self::OrderSeed) {
                128 * 1024 + 64
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
    pub location: rsi_workspace_protocol::ExecutionLocation,
    pub path: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameRequest {
    pub id: WorkspaceId,
    pub name: rsi_workspace_protocol::WorkspaceName,
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

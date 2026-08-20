use std::path::PathBuf;

use crate::{AiOperationId, SessionId};

pub type Result<T> = std::result::Result<T, AgentError>;

pub(crate) fn corrupt(message: impl Into<String>) -> AgentError {
    AgentError::CorruptStore {
        message: message.into(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreErrorClass {
    SessionCorrupt,
    ReadUnavailable,
    FatalStore,
    CommitOutcomeUnknown,
    NotStoreRelated,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum AgentError {
    #[error("invalid {field}: {message}")]
    InvalidInput {
        field: &'static str,
        message: String,
    },

    #[error("session {session_id} is already bound to a different model or prompt")]
    SessionConflict { session_id: SessionId },

    #[error("AI operation {operation_id} already exists")]
    AiOperationConflict { operation_id: AiOperationId },

    #[error("agent workspace is already open: {path}")]
    WorkspaceOccupied { path: PathBuf },

    #[error("unsupported agent store version {found}; expected {expected}")]
    UnsupportedStoreVersion { found: u32, expected: u32 },

    #[error("agent store is corrupt: {message}")]
    CorruptStore { message: String },

    #[error("session {session_id} is corrupt: {message}")]
    CorruptSession {
        session_id: SessionId,
        message: String,
    },

    #[error("agent persistence failed during {operation}: {message}")]
    Persistence {
        operation: &'static str,
        message: String,
    },

    #[error("agent persistence was temporarily unavailable during {operation}: {message}")]
    ReadUnavailable {
        operation: &'static str,
        message: String,
    },

    #[error("agent persistence commit outcome is unknown during {operation}: {message}")]
    CommitOutcomeUnknown {
        operation: &'static str,
        message: String,
    },

    #[error("session {session_id} requires workspace recovery: {message}")]
    RecoveryRequired {
        session_id: SessionId,
        message: String,
    },

    #[error("agent host is terminal; drop and reopen it")]
    HostTerminal,

    #[error("agent worker stopped unexpectedly")]
    WorkerStopped,

    #[error("AI operation failed during {operation}: {message}")]
    Ai {
        operation: &'static str,
        message: String,
    },
}

impl AgentError {
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn sqlite(operation: &'static str, error: rusqlite::Error) -> Self {
        Self::Persistence {
            operation,
            message: error.to_string(),
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn sqlite_read(operation: &'static str, error: rusqlite::Error) -> Self {
        let unavailable = matches!(
            &error,
            rusqlite::Error::SqliteFailure(sqlite, _)
                if matches!(
                    sqlite.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        );
        if unavailable {
            Self::ReadUnavailable {
                operation,
                message: error.to_string(),
            }
        } else {
            Self::sqlite(operation, error)
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn commit_outcome_unknown(operation: &'static str, error: rusqlite::Error) -> Self {
        Self::CommitOutcomeUnknown {
            operation,
            message: error.to_string(),
        }
    }

    pub(crate) fn store_error_class(&self) -> StoreErrorClass {
        match self {
            Self::CorruptSession { .. } => StoreErrorClass::SessionCorrupt,
            Self::ReadUnavailable { .. } => StoreErrorClass::ReadUnavailable,
            Self::CommitOutcomeUnknown { .. } => StoreErrorClass::CommitOutcomeUnknown,
            Self::UnsupportedStoreVersion { .. }
            | Self::CorruptStore { .. }
            | Self::Persistence { .. }
            | Self::WorkerStopped => StoreErrorClass::FatalStore,
            Self::InvalidInput { .. }
            | Self::SessionConflict { .. }
            | Self::AiOperationConflict { .. }
            | Self::WorkspaceOccupied { .. }
            | Self::RecoveryRequired { .. }
            | Self::HostTerminal
            | Self::Ai { .. } => StoreErrorClass::NotStoreRelated,
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn io(operation: &'static str, error: std::io::Error) -> Self {
        Self::Persistence {
            operation,
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sqlite_busy_and_locked_are_classified_as_read_unavailable() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = AgentError::sqlite_read(
                "read test row",
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None),
            );
            assert_eq!(
                error.store_error_class(),
                StoreErrorClass::ReadUnavailable,
                "unexpected error: {error}"
            );
            assert!(error.to_string().contains("read test row"));
        }

        let corrupt = AgentError::sqlite_read(
            "read test row",
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                None,
            ),
        );
        assert_eq!(corrupt.store_error_class(), StoreErrorClass::FatalStore);

        let session_id = SessionId::new("corrupt-session").expect("session id");
        assert_eq!(
            AgentError::CorruptSession {
                session_id,
                message: "bad event".to_owned(),
            }
            .store_error_class(),
            StoreErrorClass::SessionCorrupt
        );

        let uncertain = AgentError::commit_outcome_unknown(
            "commit test transaction",
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                None,
            ),
        );
        assert_eq!(
            uncertain.store_error_class(),
            StoreErrorClass::CommitOutcomeUnknown
        );
    }
}

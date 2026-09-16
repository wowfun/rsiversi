use super::{LocalSessionHandle, Result, SessionError};
use rsi_agent_references::{ReferenceError, References};

impl LocalSessionHandle {
    pub(super) fn reference_owner(&self) -> Result<&References> {
        self.references
            .as_deref()
            .ok_or_else(|| SessionError::NotFound("Session reference owner".into()))
    }
}
pub(super) fn map_reference_error(error: ReferenceError) -> SessionError {
    match error {
        ReferenceError::Invalid(message) => SessionError::Invalid(message),
        ReferenceError::Capacity => SessionError::Capacity,
        ReferenceError::Cancelled => SessionError::ShuttingDown,
        ReferenceError::WorkerFailed => SessionError::Backend("reference worker failed".into()),
        ReferenceError::Store(error) => super::map_store_error(error),
    }
}

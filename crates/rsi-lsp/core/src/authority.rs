use crate::{Error, Result};
use rsi_execution::{ExecutionCoordinates, ExecutionLease, ExecutionLocation, ExecutionOperation};
use std::path::{Path, PathBuf};

/// Exact workspace coordinates and live source authority, supplied by the caller's owner.
#[derive(Clone)]
pub struct LanguageWorkspace {
    coordinates: ExecutionCoordinates,
    execution: Option<ExecutionLease>,
    retained: Option<std::sync::Arc<dyn Send + Sync>>,
}
impl LanguageWorkspace {
    /// Binds coordinates to the original lease; only standalone Local callers may omit it.
    pub fn new(
        coordinates: ExecutionCoordinates,
        execution: Option<ExecutionLease>,
    ) -> Result<Self> {
        match &execution {
            Some(lease) if lease.binding().location() == coordinates.location() => {}
            None if *coordinates.location() == ExecutionLocation::Local => {}
            _ => return Err(Error::Unavailable),
        }
        Ok(Self {
            coordinates,
            execution,
            retained: None,
        })
    }
    /// Retains a caller-owned finite source lifetime through actual query settlement.
    #[must_use]
    pub fn retaining(mut self, owner: std::sync::Arc<dyn Send + Sync>) -> Self {
        self.retained = Some(owner);
        self
    }
    /// Explicit native embedding using this provider's configured local capabilities.
    pub fn local(workspace: PathBuf) -> Result<Self> {
        let path = workspace
            .into_os_string()
            .into_string()
            .map_err(|_| Error::Invalid)?;
        Self::new(
            ExecutionCoordinates::new(ExecutionLocation::Local, path)
                .map_err(|_| Error::Invalid)?,
            None,
        )
    }
    pub(crate) fn path(&self) -> &Path {
        Path::new(self.coordinates.path())
    }
    pub(crate) fn execution(&self) -> Option<&ExecutionLease> {
        self.execution.as_ref()
    }
    pub(crate) fn key(&self) -> (PathBuf, u64) {
        (
            self.path().to_owned(),
            self.execution
                .as_ref()
                .map_or(0, |lease| lease.binding().lease_generation()),
        )
    }
    pub(crate) fn admit(&self) -> Result<Option<ExecutionOperation>> {
        self.execution
            .as_ref()
            .map(|lease| lease.admit().map_err(crate::process_error))
            .transpose()
    }
}

impl std::fmt::Debug for LanguageWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguageWorkspace")
            .field("coordinates", &self.coordinates)
            .field("execution", &self.execution)
            .finish_non_exhaustive()
    }
}

use crate::{Result, SessionError};
use rsi_agent_turn_protocol::{MAXIMUM_TURN_JOBS_BYTES, TurnJobsPage};
use rsi_api_protocol::{ByteBudget, ByteReservation};
use std::sync::Arc;

/// Separate finite Jobs capture and decoded-payload retention pool.
#[derive(Clone, Debug, Default)]
pub struct JobsRetention(ByteBudget);

/// Admission acquired before Jobs sampling or wire materialization.
#[derive(Debug)]
pub struct JobsCollection(ByteReservation);

#[derive(Debug)]
struct Retained {
    page: TurnJobsPage,
    _reservation: ByteReservation,
}

/// One immutable status page whose final clone owns its retained bytes.
#[derive(Clone, Debug)]
pub struct JobsSnapshot(Arc<Retained>);
impl JobsSnapshot {
    /// Borrows the complete validated page without dropping retention.
    pub fn page(&self) -> &TurnJobsPage {
        &self.0.page
    }
}
impl serde::Serialize for JobsSnapshot {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.page.serialize(serializer)
    }
}
impl JobsRetention {
    /// Current canonical payload and admitted capture bytes.
    pub fn retained_bytes(&self) -> usize {
        self.0.used()
    }
    /// Admits a bounded 256-row native list including worst-case escaped diagnostics.
    pub fn reserve_capture(&self) -> Result<JobsCollection> {
        self.0
            .reserve(8 * 1024 * 1024)
            .map(JobsCollection)
            .map_err(|_| SessionError::Capacity)
    }
    /// Admits one complete wire page before decode.
    pub fn reserve_decode(&self) -> Result<JobsCollection> {
        self.0
            .reserve(MAXIMUM_TURN_JOBS_BYTES)
            .map(JobsCollection)
            .map_err(|_| SessionError::Capacity)
    }
}
impl JobsCollection {
    /// Validates a page and retains its exact bytes, releasing unused admission.
    pub fn retain(mut self, page: TurnJobsPage) -> Result<JobsSnapshot> {
        page.validate()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        let bytes = page
            .encoded_len()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        self.0
            .shrink(bytes)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok(JobsSnapshot(Arc::new(Retained {
            page,
            _reservation: self.0,
        })))
    }
}

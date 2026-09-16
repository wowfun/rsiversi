//! Resource response ownership independent of transport and presentation lifetimes.

use super::{Result, SessionError};
use rsi_agent_session_protocol::{
    MAXIMUM_SESSION_RESOURCE_BYTES, SessionResourceResponse, ValidatedResourceResponse,
};
use rsi_api_protocol::{ByteBudget, ByteReservation};
use std::sync::Arc;

/// Shared canonical resource-byte admission for one service or remote decoder.
#[derive(Clone, Debug, Default)]
pub struct ResourceRetention(ByteBudget);
/// Reservation acquired before reading or decoding a response.
#[derive(Debug)]
pub struct ResourceCollection(ByteReservation);
#[derive(Debug)]
struct Retained {
    response: SessionResourceResponse,
    _reservation: ByteReservation,
}
/// Validated response whose reservation follows its final clone.
#[derive(Clone, Debug)]
pub struct ResourceSnapshot(Arc<Retained>);
impl ResourceSnapshot {
    /// Borrows the exact response with its Session and request binding.
    pub fn response(&self) -> &SessionResourceResponse {
        &self.0.response
    }
}
impl serde::Serialize for ResourceSnapshot {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.response.serialize(serializer)
    }
}
impl ResourceRetention {
    /// Returns canonical bytes retained by this owner and all response clones.
    pub fn retained_bytes(&self) -> usize {
        self.0.used()
    }
    /// Reserves full output capacity before work, including local Header scratch.
    pub fn reserve(&self) -> Result<ResourceCollection> {
        self.0
            .reserve(
                MAXIMUM_SESSION_RESOURCE_BYTES
                    + rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES,
            )
            .map(ResourceCollection)
            .map_err(|_| SessionError::Capacity)
    }
}
impl ResourceCollection {
    /// Validates and transfers output ownership, releasing unused capacity.
    pub fn retain(self, response: SessionResourceResponse) -> Result<ResourceSnapshot> {
        self.retain_validated(
            response
                .validated()
                .map_err(|error| SessionError::Invalid(error.to_string()))?,
        )
    }
    /// Transfers an immutable Local proof without measuring or validating it again.
    pub fn retain_validated(
        mut self,
        response: ValidatedResourceResponse,
    ) -> Result<ResourceSnapshot> {
        let (response, bytes) = response.into_parts();
        self.0
            .shrink(bytes)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok(ResourceSnapshot(Arc::new(Retained {
            response,
            _reservation: self.0,
        })))
    }
}

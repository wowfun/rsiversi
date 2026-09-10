use super::{Result, SessionError};
use rsi_agent_session_protocol::{MAXIMUM_SESSION_PROJECTION_BYTES, SessionProjectionSnapshot};
use rsi_api_protocol::{ByteBudget, ByteReservation};
use std::sync::Arc;

/// Complete extension baselines whose reservation follows the final retained clone.
pub type ProjectionStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<ProjectionSnapshot>> + Send>>;

/// Separate 64 MiB canonical-payload budget shared by one service or decoder.
#[derive(Clone, Debug, Default)]
pub struct ProjectionRetention(ByteBudget);

/// Admission held before materializing a capture or decoding a wire snapshot.
#[derive(Debug)]
pub struct ProjectionCollection(ByteReservation);

#[derive(Debug)]
struct Retained {
    snapshot: SessionProjectionSnapshot,
    _reservation: ByteReservation,
}

/// Immutable complete snapshot with final-clone byte ownership.
#[derive(Clone, Debug)]
pub struct ProjectionSnapshot(Arc<Retained>);
impl ProjectionSnapshot {
    /// Borrows the bound, validated Agent snapshot without transferring its lease.
    pub fn snapshot(&self) -> &SessionProjectionSnapshot {
        &self.0.snapshot
    }
}
impl serde::Serialize for ProjectionSnapshot {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.0.snapshot.serialize(serializer)
    }
}
impl ProjectionRetention {
    /// Current canonical bytes owned by captures and retained snapshots.
    pub fn retained_bytes(&self) -> usize {
        self.0.used()
    }
    /// Reserves complete bounded Header/domain inputs and derived output before capture.
    pub fn reserve_capture(&self) -> Result<ProjectionCollection> {
        self.0
            .reserve(
                MAXIMUM_SESSION_PROJECTION_BYTES
                    + rsi_agent_session_protocol::MAXIMUM_DOMAIN_BASELINE_BYTES
                    + rsi_agent_session_protocol::MAXIMUM_SESSION_HEADER_BYTES,
            )
            .map(ProjectionCollection)
            .map_err(|_| SessionError::Capacity)
    }
    /// Reserves the full validated output ceiling before decoding retained transport bytes.
    pub fn reserve_decode(&self) -> Result<ProjectionCollection> {
        self.0
            .reserve(MAXIMUM_SESSION_PROJECTION_BYTES)
            .map(ProjectionCollection)
            .map_err(|_| SessionError::Capacity)
    }
}
impl ProjectionCollection {
    /// Transfers exact canonical output ownership, releasing unused capture admission.
    pub fn retain(mut self, snapshot: SessionProjectionSnapshot) -> Result<ProjectionSnapshot> {
        let bytes = snapshot
            .encoded_len()
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        self.0
            .shrink(bytes)
            .map_err(|error| SessionError::Invalid(error.to_string()))?;
        Ok(ProjectionSnapshot(Arc::new(Retained {
            snapshot,
            _reservation: self.0,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{ProjectionCursor, SessionId};

    #[test]
    fn capture_capacity_and_final_clone_ownership_are_independent_of_transport() {
        let pool = ProjectionRetention::default();
        let captures = (0..9)
            .map(|_| pool.reserve_capture().unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            pool.reserve_capture(),
            Err(SessionError::Capacity)
        ));
        assert_eq!(pool.retained_bytes(), 63 * 1024 * 1024);
        drop(captures);
        assert_eq!(pool.retained_bytes(), 0);
        let snapshot = SessionProjectionSnapshot::new(
            SessionId::new("fixture").unwrap(),
            "a".repeat(64),
            "b".repeat(64),
            ProjectionCursor::Draft { revision: 0 },
            Vec::new(),
        )
        .unwrap();
        let exact = snapshot.encoded_len().unwrap();
        let retained = pool.reserve_decode().unwrap().retain(snapshot).unwrap();
        assert_eq!(pool.retained_bytes(), exact);
        let clone = retained.clone();
        drop(retained);
        assert_eq!(pool.retained_bytes(), exact);
        drop(clone);
        assert_eq!(pool.retained_bytes(), 0);
    }
}

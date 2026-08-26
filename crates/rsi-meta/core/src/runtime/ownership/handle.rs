use super::super::{CleanupReport, EventListenerId};
use super::EventOwnership;
#[cfg(test)]
use super::OnceClaim;
use std::fmt;

/// Cloneable generation-fenced ownership handle for one event listener.
#[derive(Clone)]
pub struct EventHandle {
    id: EventListenerId,
    ownership: EventOwnership,
}

impl EventHandle {
    pub(super) fn new(id: EventListenerId, ownership: EventOwnership) -> Self {
        Self { id, ownership }
    }

    /// Returns the exact Runtime-local listener identity.
    pub fn id(&self) -> EventListenerId {
        self.id
    }

    /// Removes this exact listener once and joins its effect cleanup.
    ///
    /// Once this future is first polled, removal remains Runtime-owned through
    /// the generation effect even if the caller drops the future.
    pub async fn dispose(&self) -> CleanupReport {
        self.ownership.dispose().await.0
    }

    #[cfg(test)]
    pub(super) fn begin_once_claim(&self) -> Option<OnceClaim> {
        self.ownership.begin_once_claim()
    }
}

impl fmt::Debug for EventHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventHandle")
            .field("id", &self.id)
            .field("owner", &self.ownership.removal.owner())
            .finish_non_exhaustive()
    }
}

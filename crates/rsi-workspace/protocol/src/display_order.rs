use crate::{Result, WorkspaceError, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Maximum registrations retained by the native registry.
pub const MAXIMUM_REGISTERED_WORKSPACES: usize = 16_384;

/// Shared Host display order, filtered to the caller's visible registrations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDisplayOrder {
    /// Unique registration identities; new registrations lead the order.
    pub ids: Vec<WorkspaceId>,
}

impl WorkspaceDisplayOrder {
    /// Checks external count and uniqueness before retaining an order reply.
    pub fn validate(&self) -> Result<()> {
        if self.ids.len() > MAXIMUM_REGISTERED_WORKSPACES
            || self.ids.iter().collect::<HashSet<_>>().len() != self.ids.len()
        {
            return Err(WorkspaceError::Corrupt(
                "invalid Workspace display order".into(),
            ));
        }
        Ok(())
    }
}

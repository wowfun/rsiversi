use super::{HostPort, SdkError};
use crate::CapId;

/// Owned transferable host capability.
///
/// Keep this handle within plugin lifecycle-owned state. Trusted native plugin
/// code must not leak it to a thread or global that can outlive successful
/// plugin finalization, when the host table and module mapping may be released.
pub struct Capability {
    pub(super) port: HostPort,
    pub(super) id: CapId,
}

impl Capability {
    pub(crate) const fn new(port: HostPort, id: CapId) -> Self {
        Self { port, id }
    }

    pub fn try_clone(&self) -> Result<Self, SdkError> {
        self.port.retain(self.id)?;
        Ok(Self::new(self.port, self.id))
    }
}

impl std::fmt::Debug for Capability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Capability")
            .field("issuer", &self.id.issuer)
            .field("slot", &self.id.slot)
            .field("epoch", &self.id.epoch)
            .finish_non_exhaustive()
    }
}

impl Drop for Capability {
    fn drop(&mut self) {
        let _ = self.port.release(self.id);
    }
}

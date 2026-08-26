use super::Capability;

/// Universal native call value.
#[derive(Debug, Default)]
pub struct Message {
    pub bytes: Vec<u8>,
    pub capabilities: Vec<Capability>,
}

impl Message {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            capabilities: Vec::new(),
        }
    }

    #[must_use]
    pub fn carrying(mut self, capability: Capability) -> Self {
        self.capabilities.push(capability);
        self
    }
}

use crate::{Error, Result};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(crate) const PAYLOAD_BUDGET: usize = 8 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct Budget(AtomicUsize);
impl Budget {
    fn charge(self: &Arc<Self>, bytes: usize) -> Result<Charge> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= PAYLOAD_BUDGET)
            })
            .map_err(|_| Error::Capacity)?;
        Ok(Charge {
            budget: self.clone(),
            bytes,
        })
    }
}

#[derive(Debug)]
struct Charge {
    budget: Arc<Budget>,
    bytes: usize,
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// A complete message retaining its share of the connection's receive budget.
pub struct Message {
    bytes: Vec<u8>,
    charge: Charge,
}
impl std::fmt::Debug for Message {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Message")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}
impl Message {
    pub(crate) fn new(bytes: Vec<u8>, budget: &Arc<Budget>) -> Result<Self> {
        // Copy to exact capacity so a caller cannot retain an oversized allocation
        // behind a small admitted length.
        let charge = budget.charge(bytes.len())?;
        Ok(Self {
            bytes: bytes.into_boxed_slice().into_vec(),
            charge,
        })
    }
    pub(crate) fn extend(&mut self, bytes: &[u8]) -> Result<()> {
        let charge = self.charge.budget.charge(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        self.charge.bytes += charge.bytes;
        // Transfer the charge without letting its destructor return those bytes.
        let mut charge = charge;
        charge.bytes = 0;
        Ok(())
    }
    /// Borrows the complete opaque payload; the helper owns schema validation.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}
